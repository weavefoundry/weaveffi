//! Node.js (N-API) binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader plus TypeScript type definitions for the
//! companion N-API addon. Async functions surface as `Promise`-returning
//! functions, `iter<T>` functions surface as lazy `IterableIterator<T>`
//! wrappers that pull one element per step, interfaces surface as JS classes
//! over opaque native handles, and each declared error domain surfaces as an
//! `Error` subclass extending the generic `WeaveFFIError` brand. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::collections::HashMap;

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToSnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, is_c_pointer_type, DocCommentStyle,
};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::{type_name as error_type_name, ERROR_BRAND};
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
    StructBinding,
};
use weaveffi_core::plan::ElemFree;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_json_prelude, render_prelude, render_trailer,
    wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`NodeGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// npm package name (default `"weaveffi"`).
    pub package_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted JS/TS function names, so module `kv`'s `open_store` exports as
    /// `openStore` rather than `kvOpenStore`. Set to `false` to keep
    /// module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the native addon calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl NodeConfig {
    /// Returns the configured npm package name, falling back to `"weaveffi"`.
    pub fn package_name(&self) -> &str {
        self.package_name.as_deref().unwrap_or("weaveffi")
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

/// Node.js backend: emits a JavaScript loader and TypeScript declarations for
/// the companion N-API addon that wraps the C ABI.
pub struct NodeGenerator;

impl LanguageBackend for NodeGenerator {
    type Config = NodeConfig;

    fn name(&self) -> &'static str {
        "node"
    }

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
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let strip = config.strip_module_prefix;
        vec![
            OutputFile::new(
                dir.join("index.js"),
                render_node_index(model, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("types.d.ts"),
                render_node_dts(model, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("package.json"),
                render_package_json(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
            OutputFile::new(dir.join("binding.gyp"), render_binding_gyp(input_basename)),
            OutputFile::new(
                dir.join("weaveffi_addon.c"),
                render_addon_c(model, strip, input_basename),
            ),
        ]
    }

    fn package(
        &self,
        api: &Api,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let strip = config.strip_module_prefix;
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib = &ctx.binaries.lib_name;

        // The per-platform package names follow the esbuild/swc convention:
        // `<pkg>-<node-os>-<node-cpu>` constrained by npm `os`/`cpu`, so npm
        // installs only the matching one.
        let platform_pkgs: Vec<(weaveffi_core::platform::Platform, String)> = ctx
            .binaries
            .platforms()
            .map(|p| {
                (
                    p,
                    format!("{}-{}-{}", package.name, p.node_os(), p.node_cpu()),
                )
            })
            .collect();

        let mut files = vec![
            PackagedFile::text(
                dir.join("index.js"),
                render_node_index(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("types.d.ts"),
                render_node_dts(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("package.json"),
                render_packaged_package_json(&package, &platform_pkgs, input_basename),
            ),
            PackagedFile::text(
                dir.join("binding.gyp"),
                render_packaged_binding_gyp(&package.name, lib, input_basename),
            ),
            PackagedFile::text(
                dir.join("weaveffi_addon.c"),
                render_addon_c(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];

        // Each platform package bundles its prebuilt library and is gated by
        // npm `os`/`cpu` so only the matching one installs.
        for (platform, pkg_name) in &platform_pkgs {
            let pkg_dir = dir.join("npm").join(pkg_name);
            files.push(PackagedFile::text(
                pkg_dir.join("package.json"),
                render_platform_package_json(pkg_name, &package.version, *platform),
            ));
            let nb = ctx.binaries.get(*platform).expect("platform has a binary");
            files.push(PackagedFile::copy(
                pkg_dir.join(ctx.binaries.bundled_filename(*platform)),
                nb.source.clone(),
            ));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(NodeGenerator);

/// Render the main package's `package.json` with `optionalDependencies` on the
/// per-platform native packages.
fn render_packaged_package_json(
    package: &ResolvedPackage,
    platform_pkgs: &[(weaveffi_core::platform::Platform, String)],
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
    if let Some(repository) = &package.repository {
        optional.push_str(&format!(
            "  \"repository\": {{ \"type\": \"git\", \"url\": \"{repository}\" }},\n"
        ));
    }
    let deps = platform_pkgs
        .iter()
        .map(|(_, pkg_name)| format!("    \"{pkg_name}\": \"{version}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n{prelude}  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"{description}\",\n{optional}  \"main\": \"index.js\",\n  \"types\": \"types.d.ts\",\n  \"gypfile\": true,\n  \"scripts\": {{\n    \"install\": \"node-gyp rebuild\"\n  }},\n  \"optionalDependencies\": {{\n{deps}\n  }}\n}}\n"
    )
}

/// Render a per-platform native package's `package.json`, gated by npm `os` and
/// `cpu` so npm installs only the matching one.
fn render_platform_package_json(
    pkg_name: &str,
    version: &str,
    platform: weaveffi_core::platform::Platform,
) -> String {
    let os = platform.node_os();
    let cpu = platform.node_cpu();
    format!(
        "{{\n  \"name\": \"{pkg_name}\",\n  \"version\": \"{version}\",\n  \"description\": \"Prebuilt WeaveFFI native library for {os}/{cpu}\",\n  \"os\": [\"{os}\"],\n  \"cpu\": [\"{cpu}\"]\n}}\n"
    )
}

/// Render the packaged `binding.gyp`: it links the prebuilt library resolved
/// from the installed per-platform package (selected by npm `os`/`cpu`) and
/// sets an rpath so the addon finds it at runtime.
fn render_packaged_binding_gyp(pkg_name: &str, lib: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "binding.gyp");
    let resolve = format!(
        "<!(node -p \"require('path').dirname(require.resolve('{pkg_name}-' + process.platform + '-' + process.arch + '/package.json'))\")"
    );
    let mut out = String::new();
    out.push_str(&prelude);
    out.push_str("{\n");
    out.push_str("  \"variables\": {\n");
    out.push_str(&format!("    \"wv_native_dir%\": \"{resolve}\"\n"));
    out.push_str("  },\n");
    out.push_str("  \"targets\": [\n");
    out.push_str("    {\n");
    out.push_str("      \"target_name\": \"weaveffi\",\n");
    out.push_str("      \"sources\": [\"weaveffi_addon.c\"],\n");
    out.push_str("      \"include_dirs\": [\"../c\"],\n");
    out.push_str("      \"library_dirs\": [\"<(wv_native_dir)\"],\n");
    out.push_str(&format!("      \"libraries\": [\"-l{lib}\"],\n"));
    out.push_str("      \"conditions\": [\n");
    out.push_str("        [\"OS=='mac'\", { \"xcode_settings\": { \"OTHER_LDFLAGS\": [\"-Wl,-rpath,<(wv_native_dir)\"] } }],\n");
    out.push_str("        [\"OS=='linux'\", { \"ldflags\": [\"-Wl,-rpath,<(wv_native_dir)\"] }]\n");
    out.push_str("      ]\n");
    out.push_str("    }\n");
    out.push_str("  ]\n");
    out.push_str("}\n\n");
    out.push_str(&trailer);
    out
}

/// README for a packaged Node artifact using `optionalDependencies`.
fn render_packaged_readme(
    package: &ResolvedPackage,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `{}-{}-{}`", name, p.node_os(), p.node_cpu()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {name} (Node.js)

Auto-generated N-API bindings. The prebuilt native library is published as a set
of per-platform packages and selected automatically through
`optionalDependencies` (npm installs only the package matching the host
`os`/`cpu`):

{platform_list}

The thin N-API addon is compiled at install time (`node-gyp rebuild`) and links
the prebuilt library from the selected platform package, so no Rust toolchain is
needed. A C compiler and the generated C header (`../c`) are required to build
the addon.

{trailer}"#,
    )
}

fn render_package_json(package: &ResolvedPackage, input_basename: &str) -> String {
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
    if let Some(repository) = &package.repository {
        optional.push_str(&format!(
            "  \"repository\": {{ \"type\": \"git\", \"url\": \"{repository}\" }},\n"
        ));
    }
    format!(
        "{{\n{prelude}  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"{description}\",\n{optional}  \"main\": \"index.js\",\n  \"types\": \"types.d.ts\",\n  \"gypfile\": true,\n  \"scripts\": {{\n    \"install\": \"node-gyp rebuild\"\n  }}\n}}\n"
    )
}

fn render_binding_gyp(input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "binding.gyp");
    format!(
        "{prelude}{{\n  \"targets\": [\n    {{\n      \"target_name\": \"weaveffi\",\n      \"sources\": [\"weaveffi_addon.c\"],\n      \"include_dirs\": [\"../c\"],\n      \"libraries\": [\"-lweaveffi\"]\n    }}\n  ]\n}}\n\n{trailer}"
    )
}

/// The exported JS name of a free function or listener endpoint:
/// [`wrapper_name`] (module-prefixed or stripped per config) converted to
/// lowerCamelCase, so module `kv`'s `open_store` exports as `openStore`
/// (stripped, the default) or `kvOpenStore`.
fn js_fn_name(module: &str, func: &str, strip: bool) -> String {
    wrapper_name(module, func, strip).to_lower_camel_case()
}

/// The camelCase JS spelling of an IDL parameter name.
fn js_param_name(name: &str) -> String {
    name.to_lower_camel_case()
}

/// The addon-internal JS export base of an interface member
/// (`{Interface}_{member}`). These names are wiring between the addon and the
/// generated classes, not public API, so they keep the raw member spelling
/// exactly like the rich-enum helper exports.
fn iface_member_base(iface: &str, member: &str) -> String {
    format!("{iface}_{member}")
}

/// Escape a string for embedding in a single-quoted JS literal.
fn js_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
}

fn c_elem_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I8 => "int8_t".into(),
        TypeRef::I16 => "int16_t".into(),
        TypeRef::I32 => "int32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::U8 => "uint8_t".into(),
        TypeRef::U16 => "uint16_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::U64 => "uint64_t".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        // A generic `handle` is an opaque integer; a typed `handle<T>` is the C
        // ABI struct pointer for T (same lowering as a struct value), so it must
        // carry T's owner-qualified symbol, not the generic integer type.
        TypeRef::Handle => "weaveffi_handle_t".into(),
        TypeRef::TypedHandle(s) => format!("{}*", c_abi_struct_name(s, module, prefix)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*".into(),
        // Records, rich enums, and interfaces share the opaque-pointer
        // lowering; only the ownership convention differs, which element
        // contexts don't touch.
        TypeRef::Record(s) | TypeRef::RichEnum(s) | TypeRef::Interface(s) => {
            format!("{}*", c_abi_struct_name(s, module, prefix))
        }
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            c_elem_type(inner, module, prefix)
        }
        TypeRef::Map(_, _) => "void*".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn c_ret_type_str(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I8 => "int8_t".into(),
        TypeRef::I16 => "int16_t".into(),
        TypeRef::I32 => "int32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::U8 => "uint8_t".into(),
        TypeRef::U16 => "uint16_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::U64 => "uint64_t".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*".into(),
        TypeRef::Handle => "weaveffi_handle_t".into(),
        TypeRef::TypedHandle(s) => format!("{}*", c_abi_struct_name(s, module, prefix)),
        // A returned interface transfers ownership of a new object reference;
        // the pointer spelling matches a record return.
        TypeRef::Record(s) | TypeRef::RichEnum(s) | TypeRef::Interface(s) => {
            format!("{}*", c_abi_struct_name(s, module, prefix))
        }
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) => {
            // Pointer returns stay nullable pointers (null = none); scalar
            // returns are boxed by the producer, matching the ABI lowering.
            if is_c_pointer_type(inner) {
                c_ret_type_str(inner, module, prefix)
            } else {
                format!("{}*", c_elem_type(inner, module, prefix))
            }
        }
        TypeRef::List(inner) => format!("{}*", c_elem_type(inner, module, prefix)),
        TypeRef::Map(_, _) => "void".into(),
        TypeRef::Iterator(_) => "void*".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn napi_getter(ty: &TypeRef) -> &'static str {
    match ty {
        // i8/i16 are read through the 32-bit signed getter (N-API has no
        // narrower int getter) and narrowed at the use site.
        TypeRef::I8 | TypeRef::I16 | TypeRef::I32 | TypeRef::Enum(_) => "napi_get_value_int32",
        TypeRef::U8 | TypeRef::U16 | TypeRef::U32 => "napi_get_value_uint32",
        // u64 mirrors i64/handle: read as a 64-bit int, reinterpreted as needed.
        TypeRef::I64
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::TypedHandle(_)
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_) => "napi_get_value_int64",
        // f32 is read as a double then narrowed to float at the use site.
        TypeRef::F32 | TypeRef::F64 => "napi_get_value_double",
        TypeRef::Bool => "napi_get_value_bool",
        _ => "napi_get_value_int64",
    }
}

/// The C type of the temporary an N-API getter writes into for a scalar that is
/// narrower than the getter's natural width. N-API only exposes 32/64-bit int
/// and `double` getters, so `i8/i16/u8/u16/f32` must be read into a wider
/// temporary and then narrowed with an explicit cast to the real ABI type
/// returned by [`c_elem_type`]; `u64` is read as `int64_t` then reinterpreted.
fn napi_read_tmp_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 | TypeRef::I16 => "int32_t",
        TypeRef::U8 | TypeRef::U16 => "uint32_t",
        TypeRef::U64 => "int64_t",
        TypeRef::F32 => "double",
        _ => "int64_t",
    }
}

/// Whether `ty` is one of the numeric primitives narrower or wider than what an
/// N-API number getter writes directly, requiring a temporary + cast on read.
fn needs_narrowing_read(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I8 | TypeRef::I16 | TypeRef::U8 | TypeRef::U16 | TypeRef::U64 | TypeRef::F32
    )
}

/// Emit `{prefix}_napi_error_value`, the shared constructor of the JS error
/// object every failure path produces: a plain `Error` carrying the numeric
/// ABI code as a `code` property. The JS loader rebrands it as the generic
/// `WeaveFFIError` or the module's typed domain class.
fn render_error_value_helper_c(out: &mut String, prefix: &str) {
    out.push_str(&format!(
        "static napi_value {prefix}_napi_error_value(napi_env env, int32_t code, const char* message) {{\n"
    ));
    out.push_str("    napi_value msg;\n");
    out.push_str(
        "    napi_create_string_utf8(env, message ? message : \"\", NAPI_AUTO_LENGTH, &msg);\n",
    );
    out.push_str("    napi_value err;\n");
    out.push_str("    napi_create_error(env, NULL, msg, &err);\n");
    out.push_str("    napi_value code_val;\n");
    out.push_str("    napi_create_int32(env, code, &code_val);\n");
    out.push_str("    napi_set_named_property(env, err, \"code\", code_val);\n");
    out.push_str("    return err;\n");
    out.push_str("}\n\n");
}

/// Emit the post-call `out_err` check: throw the code-carrying JS error and
/// bail on a non-zero slot. The JS loader maps the `code` property to the
/// module's typed domain class (throwing callables) or the generic brand.
fn emit_error_check_c(out: &mut String, prefix: &str) {
    out.push_str("  if (err.code != 0) {\n");
    out.push_str(&format!(
        "    napi_throw(env, {prefix}_napi_error_value(env, err.code, err.message));\n"
    ));
    out.push_str("    weaveffi_error_clear(&err);\n");
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");
}

/// Emit the shared state cell every lazy iterator external wraps. The cell
/// owns the native iterator handle; `next` on exhaustion, the JS wrapper's
/// `return()`, and the external's finalizer all null it before destroying,
/// so the handle is destroyed exactly once no matter which path runs first.
fn render_iter_state_c(out: &mut String, prefix: &str) {
    out.push_str("typedef struct {\n");
    out.push_str("    void* iter;\n");
    out.push_str(&format!("}} {prefix}_napi_iter_state;\n\n"));
}

/// Read the iterator state cell back out of the external in `args[0]`.
fn emit_iter_state_read(out: &mut String, prefix: &str) {
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  void* iter_data = NULL;\n");
    out.push_str("  napi_get_value_external(env, args[0], &iter_data);\n");
    out.push_str(&format!(
        "  {prefix}_napi_iter_state* state = ({prefix}_napi_iter_state*)iter_data;\n"
    ));
}

/// Emit one iterator-returning callable's lazy machinery: the external's
/// finalizer (the safety net for abandoned iterators), the per-step `next`
/// entry point, and the explicit `destroy` entry point the JS wrapper's
/// `return()` calls on early exit.
///
/// `next` issues exactly one native pull. When the producer reports done (or
/// faults), the native handle is destroyed eagerly and the cell nulled; a
/// per-step fault then throws the code-carrying error, which the JS wrapper
/// maps per the callable's error strategy. A produced element is converted
/// and released per its element plan: strings are freed with
/// `weaveffi_free_string` after the JS string is created, record pointers are
/// adopted by the struct-object materialization (which destroys them), and
/// rich-enum pointers surface as owned raw handles the JS class adopts.
fn render_iterator_napi_fns(
    out: &mut String,
    f: &FnBinding,
    ib: &IteratorBinding,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
) {
    let c_name = &f.c_base;
    let tag = &ib.iter_tag;
    let next_sym = &ib.next.symbol;
    let destroy_sym = &ib.destroy_symbol;
    let proto = ib.protocol(f, module, prefix);

    // Finalizer: reclaim abandoned iterators when the external is collected.
    out.push_str(&format!(
        "static void {c_name}_napi_iter_finalize(napi_env env, void* data, void* hint) {{\n"
    ));
    out.push_str("    (void)env;\n");
    out.push_str("    (void)hint;\n");
    out.push_str(&format!(
        "    {prefix}_napi_iter_state* state = ({prefix}_napi_iter_state*)data;\n"
    ));
    out.push_str("    if (state->iter != NULL) {\n");
    out.push_str(&format!("        {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("        state->iter = NULL;\n");
    out.push_str("    }\n");
    out.push_str("    free(state);\n");
    out.push_str("}\n\n");

    // One pull per call; `undefined` signals exhaustion to the JS wrapper.
    out.push_str(&format!(
        "static napi_value Napi_{next_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_iter_state_read(out, prefix);
    out.push_str("  napi_value ret;\n");
    out.push_str("  if (state == NULL || state->iter == NULL) {\n");
    out.push_str("    napi_get_undefined(env, &ret);\n");
    out.push_str("    return ret;\n");
    out.push_str("  }\n");
    let et = c_elem_type(&ib.elem, module, prefix);
    out.push_str(&format!("  {et} iter_item;\n"));
    out.push_str("  weaveffi_error iter_err = {0};\n");
    out.push_str(&format!(
        "  if (!{next_sym}(({tag}*)state->iter, &iter_item, &iter_err)) {{\n"
    ));
    out.push_str(&format!("    {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("    state->iter = NULL;\n");
    out.push_str("    if (iter_err.code != 0) {\n");
    out.push_str(&format!(
        "      napi_throw(env, {prefix}_napi_error_value(env, iter_err.code, iter_err.message));\n"
    ));
    out.push_str("      weaveffi_error_clear(&iter_err);\n");
    out.push_str("      return NULL;\n");
    out.push_str("    }\n");
    out.push_str("    napi_get_undefined(env, &ret);\n");
    out.push_str("    return ret;\n");
    out.push_str("  }\n");
    match &ib.elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(
                "  napi_create_string_utf8(env, iter_item ? iter_item : \"\", NAPI_AUTO_LENGTH, &ret);\n",
            );
            if matches!(proto.elem_free, ElemFree::String) {
                out.push_str("  weaveffi_free_string((char*)iter_item);\n");
            }
        }
        TypeRef::Record(name) => {
            emit_struct_to_object(
                out, "env", name, "iter_item", "ret", module, prefix, structs, "  ", true,
            );
        }
        TypeRef::RichEnum(_) => {
            out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)iter_item, &ret);\n");
        }
        other => {
            out.push_str(&format!(
                "  {}\n",
                napi_create_leaf("env", other, "iter_item", "ret")
            ));
        }
    }
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    // Explicit destroy, guarded so destroy-after-exhaustion (or a double
    // `return()`) is a no-op rather than a double free.
    out.push_str(&format!(
        "static napi_value Napi_{destroy_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_iter_state_read(out, prefix);
    out.push_str("  if (state != NULL && state->iter != NULL) {\n");
    out.push_str(&format!("    {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("    state->iter = NULL;\n");
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

/// Emit one callable's `Napi_*` entry point (plus its async or iterator
/// machinery when needed) and register its JS export(s). `self_tag` is the
/// interface `c_tag` for an instance method, whose wrapped pointer arrives as
/// `args[0]`. An iterator-returning callable additionally exports its
/// per-iterator `next`/`destroy` entry points under `{js_name}_iterNext` and
/// `{js_name}_iterDestroy`, which the JS wrapper drives lazily.
#[allow(clippy::too_many_arguments)]
fn render_callable_napi(
    out: &mut String,
    all_exports: &mut Vec<(String, String)>,
    f: &FnBinding,
    js_name: String,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
    self_tag: Option<&str>,
) {
    let c_name = &f.c_base;
    let napi_name = format!("Napi_{c_name}");

    if f.is_async {
        render_async_machinery(out, f, c_name, module, prefix, structs);
    }
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator_napi_fns(out, f, ib, module, prefix, structs);
        all_exports.push((
            format!("{js_name}_iterNext"),
            format!("Napi_{}", ib.next.symbol),
        ));
        all_exports.push((
            format!("{js_name}_iterDestroy"),
            format!("Napi_{}", ib.destroy_symbol),
        ));
    }
    all_exports.push((js_name, napi_name.clone()));

    out.push_str(&format!(
        "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
    ));
    if f.is_async {
        render_async_napi_body(out, f, module, prefix, self_tag);
    } else {
        render_napi_body(out, f, module, prefix, structs, self_tag);
    }
    out.push_str("}\n\n");
}

fn render_addon_c(model: &BindingModel, strip_module_prefix: bool, input_basename: &str) -> String {
    let prefix = model.prefix.as_str();
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str(&format!(
        "#include <node_api.h>\n#include \"{prefix}.h\"\n#include <stdlib.h>\n#include <string.h>\n\n"
    ));

    let mut all_exports: Vec<(String, String)> = Vec::new();
    let structs = struct_registry(model);

    // Every error path (sync throws, async rejections, rich-enum constructor
    // failures) funnels through one code-carrying error constructor.
    let has_error_paths = model.modules.iter().any(|m| {
        !m.functions.is_empty() || !m.interfaces.is_empty() || m.enums.iter().any(|e| e.is_rich())
    });
    if has_error_paths {
        render_error_value_helper_c(&mut out, prefix);
    }

    if model_has_iterators(model) {
        render_iter_state_c(&mut out, prefix);
    }

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        render_listener_support_c(&mut out, prefix);
    }

    for m in &model.modules {
        // Rich (algebraic) enums cross the ABI as opaque objects, so they get a
        // struct-like native surface: a tag reader, per-variant constructors and
        // field getters, and a destructor. (Plain C-style enums cross by value
        // as int32 and need no native helpers.)
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum_napi_fns(
                    &mut out,
                    e,
                    &m.path,
                    prefix,
                    strip_module_prefix,
                    &structs,
                    &mut all_exports,
                );
            }
        }
        // Interfaces get one native entry point per member (constructors and
        // statics marshal like free functions; methods additionally read the
        // wrapped pointer from the leading argument) plus the destructor the
        // JS class's disposal path calls.
        for i in &m.interfaces {
            for f in i.constructors.iter().chain(i.statics.iter()) {
                render_callable_napi(
                    &mut out,
                    &mut all_exports,
                    f,
                    wrapper_name(
                        &m.path,
                        &iface_member_base(&i.name, &f.name),
                        strip_module_prefix,
                    ),
                    &m.path,
                    prefix,
                    &structs,
                    None,
                );
            }
            for f in &i.methods {
                render_callable_napi(
                    &mut out,
                    &mut all_exports,
                    f,
                    wrapper_name(
                        &m.path,
                        &iface_member_base(&i.name, &f.name),
                        strip_module_prefix,
                    ),
                    &m.path,
                    prefix,
                    &structs,
                    Some(&i.c_tag),
                );
            }
            render_interface_destroy_napi(&mut out, i);
            all_exports.push((
                wrapper_name(
                    &m.path,
                    &iface_member_base(&i.name, "destroy"),
                    strip_module_prefix,
                ),
                format!("Napi_{}", i.destroy_symbol),
            ));
        }
        // Callbacks referenced by listeners get a payload struct, a producer-
        // thread trampoline, and a JS-thread marshaller (threadsafe function).
        let used_callbacks: Vec<&CallbackBinding> = m
            .listeners
            .iter()
            .filter_map(|l| m.callback(&l.event_callback))
            .collect();
        for cb in &used_callbacks {
            render_cb_payload_struct(&mut out, cb, prefix);
            render_cb_tramp(&mut out, cb, prefix);
            render_cb_calljs(&mut out, cb, prefix);
        }
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                unreachable!("validation guarantees the listener's callback exists");
            };
            render_listener_napi_fns(&mut out, l, cb, prefix);
            all_exports.push((
                js_fn_name(
                    &m.path,
                    &format!("register_{}", l.name),
                    strip_module_prefix,
                ),
                format!("Napi_{}", l.register_symbol),
            ));
            all_exports.push((
                js_fn_name(
                    &m.path,
                    &format!("unregister_{}", l.name),
                    strip_module_prefix,
                ),
                format!("Napi_{}", l.unregister_symbol),
            ));
        }
        for f in &m.functions {
            render_callable_napi(
                &mut out,
                &mut all_exports,
                f,
                js_fn_name(&m.path, &f.name, strip_module_prefix),
                &m.path,
                prefix,
                &structs,
                None,
            );
        }
    }

    out.push_str("static napi_value Init(napi_env env, napi_value exports) {\n");
    if !all_exports.is_empty() {
        out.push_str("  napi_property_descriptor props[] = {\n");
        for (js_name, napi_fn) in &all_exports {
            out.push_str(&format!(
                "    {{ \"{js_name}\", NULL, {napi_fn}, NULL, NULL, NULL, napi_default, NULL }},\n"
            ));
        }
        out.push_str("  };\n");
        out.push_str(&format!(
            "  napi_define_properties(env, exports, {}, props);\n",
            all_exports.len()
        ));
    }
    out.push_str("  return exports;\n");
    out.push_str("}\n\n");
    out.push_str("NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)\n\n");
    out.push_str(&render_trailer(
        CommentStyle::DoubleSlash,
        "weaveffi_addon.c",
    ));
    out
}

// --- Rich (algebraic) enum support -----------------------------------------
//
// A rich enum crosses the ABI exactly like a struct: an opaque object pointer
// surfaced to JS as the same int64 handle structs use. The JS-export base names
// below are shared by the addon (which exports the native helpers) and the JS
// loader (whose `Shape` class calls them), so both halves agree by construction.

/// `{Enum}_tag`, the JS-export base for a rich enum's discriminant reader.
fn rich_tag_base(enum_name: &str) -> String {
    format!("{enum_name}_tag")
}

/// `{Enum}_{variant}_new`, the JS-export base for a variant constructor.
fn rich_ctor_base(enum_name: &str, variant: &str) -> String {
    format!("{enum_name}_{}_new", variant.to_snake_case())
}

/// `{Enum}_{variant}_get_{field}`, the JS-export base for a field getter.
fn rich_getter_base(enum_name: &str, variant: &str, field: &str) -> String {
    format!("{enum_name}_{}_get_{field}", variant.to_snake_case())
}

/// `{Enum}_destroy`, the JS-export base for the destructor.
fn rich_destroy_base(enum_name: &str) -> String {
    format!("{enum_name}_destroy")
}

/// Read `args[0]` as the opaque handle and bind it to a typed `self` pointer.
/// Shared by the tag reader, every field getter, and the destructor.
fn emit_rich_self_read(out: &mut String, c_tag: &str) {
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  int64_t self_raw;\n");
    out.push_str("  napi_get_value_int64(env, args[0], &self_raw);\n");
    out.push_str(&format!(
        "  {c_tag}* self = ({c_tag}*)(intptr_t)self_raw;\n"
    ));
}

/// Emit the native helpers for one rich enum and register their JS exports:
/// the tag reader, one constructor per variant, one getter per variant field,
/// and the destructor. Constructors reuse [`emit_param`] (the struct-create
/// marshalling) for their arguments; getters reuse [`emit_struct_field_to_napi`]
/// (the struct-field marshalling), so strings/numerics/bytes/lists are
/// materialized identically to struct fields.
#[allow(clippy::too_many_arguments)]
fn render_rich_enum_napi_fns(
    out: &mut String,
    e: &EnumBinding,
    module: &str,
    prefix: &str,
    strip: bool,
    structs: &HashMap<String, StructBinding>,
    all_exports: &mut Vec<(String, String)>,
) {
    let Some(rich) = &e.rich else {
        return;
    };
    let c_tag = &e.c_tag;
    let name = &e.name;

    // tag reader: int32 discriminant of the active variant.
    let napi_tag = format!("Napi_{}", rich.tag_symbol);
    out.push_str(&format!(
        "static napi_value {napi_tag}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_rich_self_read(out, c_tag);
    out.push_str("  napi_value ret;\n");
    out.push_str(&format!(
        "  napi_create_int32(env, {}(self), &ret);\n",
        rich.tag_symbol
    ));
    out.push_str("  return ret;\n}\n\n");
    all_exports.push((wrapper_name(module, &rich_tag_base(name), strip), napi_tag));

    // One constructor per variant: read each variant field as a JS argument
    // (reusing the struct-create marshalling), call `{Enum}_{V}_new`, and return
    // the resulting owned pointer as the int64 handle.
    for v in &rich.variants {
        let napi_ctor = format!("Napi_{}", v.create.symbol);
        out.push_str(&format!(
            "static napi_value {napi_ctor}(napi_env env, napi_callback_info info) {{\n"
        ));
        let n = v.fields.len();
        if n > 0 {
            out.push_str(&format!("  size_t argc = {n};\n"));
            out.push_str(&format!("  napi_value args[{n}];\n"));
            out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
        } else {
            out.push_str("  size_t argc = 0;\n");
            out.push_str("  napi_get_cb_info(env, info, &argc, NULL, NULL, NULL);\n");
        }
        let mut c_args: Vec<String> = Vec::new();
        let mut cleanups: Vec<String> = Vec::new();
        for (i, f) in v.fields.iter().enumerate() {
            emit_param(
                out,
                &mut c_args,
                &mut cleanups,
                &f.ty,
                &f.name,
                i,
                module,
                prefix,
            );
        }
        out.push_str("  weaveffi_error err = {0};\n");
        c_args.push("&err".to_string());
        out.push_str(&format!(
            "  {c_tag}* result = {}({});\n",
            v.create.symbol,
            c_args.join(", ")
        ));
        for cleanup in &cleanups {
            out.push_str(cleanup);
        }
        emit_error_check_c(out, prefix);
        out.push_str("  napi_value ret;\n");
        out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
        out.push_str("  return ret;\n}\n\n");
        all_exports.push((
            wrapper_name(module, &rich_ctor_base(name, &v.name), strip),
            napi_ctor,
        ));
    }

    // One getter per variant field, namespaced by variant. Reuses the struct
    // field marshalling, so the active variant's payload surfaces exactly like
    // a struct field would (string decode + free, Buffer copy, list/array, …).
    for v in &rich.variants {
        for f in &v.fields {
            let napi_getter = format!("Napi_{}", f.getter_symbol);
            out.push_str(&format!(
                "static napi_value {napi_getter}(napi_env env, napi_callback_info info) {{\n"
            ));
            emit_rich_self_read(out, c_tag);
            out.push_str("  napi_value ret;\n");
            emit_struct_field_to_napi(
                out,
                "env",
                &f.ty,
                &f.getter_symbol,
                "self",
                "ret",
                module,
                prefix,
                structs,
                "  ",
            );
            out.push_str("  return ret;\n}\n\n");
            all_exports.push((
                wrapper_name(module, &rich_getter_base(name, &v.name, &f.name), strip),
                napi_getter,
            ));
        }
    }

    // Destructor: free the opaque object behind the handle.
    let napi_destroy = format!("Napi_{}", rich.destroy_symbol);
    out.push_str(&format!(
        "static napi_value {napi_destroy}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_rich_self_read(out, c_tag);
    out.push_str(&format!("  {}(self);\n", rich.destroy_symbol));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n}\n\n");
    all_exports.push((
        wrapper_name(module, &rich_destroy_base(name), strip),
        napi_destroy,
    ));
}

/// The `Napi_*` destructor entry point for one interface: reads the wrapped
/// pointer from `args[0]` and releases the object via the destroy symbol.
/// Called by the JS class's `destroy()` and its `FinalizationRegistry` net.
fn render_interface_destroy_napi(out: &mut String, i: &InterfaceBinding) {
    let napi_destroy = format!("Napi_{}", i.destroy_symbol);
    out.push_str(&format!(
        "static napi_value {napi_destroy}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_rich_self_read(out, &i.c_tag);
    out.push_str(&format!("  {}(self);\n", i.destroy_symbol));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n}\n\n");
}

/// The listener context + registry shared by every generated listener. The
/// registry is only mutated from the JS thread (register/unregister are plain
/// N-API calls), so a simple singly-linked list suffices.
fn render_listener_support_c(out: &mut String, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.block(
        format!("typedef struct {prefix}_napi_listener_ctx {{"),
        format!("}} {prefix}_napi_listener_ctx;"),
        |w| {
            w.line("napi_threadsafe_function tsfn;");
            w.line("uint64_t id;");
            w.line(format!("struct {prefix}_napi_listener_ctx* next;"));
        },
    );
    w.blank();
    w.line(format!(
        "static {prefix}_napi_listener_ctx* {prefix}_napi_listeners = NULL;"
    ));
    w.blank();
    out.push_str(&w.finish());
}

fn cb_payload_name(cb: &CallbackBinding) -> String {
    format!("{}_payload", cb.c_fn_type)
}

/// The C slot declarations of a callback's parameters (without context).
fn cb_slot_decls(cb: &CallbackBinding, prefix: &str) -> Vec<String> {
    cb.params
        .iter()
        .flat_map(|p| abi::lower_param(&p.name, &p.ty, "", false))
        .map(|slot| format!("{} {}", slot.ty.render_c(prefix), slot.name))
        .collect()
}

/// The deep-copy payload carried from the producer thread to the JS thread.
/// Every pointer field is owned by the payload (strdup/memcpy in the
/// trampoline, freed in the call-js marshaller); struct/handle pointers are
/// shallow-copied and surface as numeric handles.
fn render_cb_payload_struct(out: &mut String, cb: &CallbackBinding, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.block(
        "typedef struct {",
        format!("}} {};", cb_payload_name(cb)),
        |w| {
            for p in &cb.params {
                let slots = abi::lower_param(&p.name, &p.ty, "", false);
                let n0 = &slots[0].name;
                match &p.ty {
                    TypeRef::I8
                    | TypeRef::I16
                    | TypeRef::I32
                    | TypeRef::I64
                    | TypeRef::U8
                    | TypeRef::U16
                    | TypeRef::U32
                    | TypeRef::U64
                    | TypeRef::F32
                    | TypeRef::F64
                    | TypeRef::Bool
                    | TypeRef::Handle
                    | TypeRef::Enum(_) => {
                        w.line(format!("{} {n0};", slots[0].ty.render_c(prefix)));
                    }
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                        w.line(format!("char* {n0};"));
                    }
                    TypeRef::Bytes | TypeRef::BorrowedBytes => {
                        w.line(format!("uint8_t* {n0};"));
                        w.line(format!("size_t {};", slots[1].name));
                    }
                    TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => {
                        w.line(format!("void* {n0};"));
                    }
                    TypeRef::Optional(inner) => match inner.as_ref() {
                        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                            w.line(format!("char* {n0};"));
                        }
                        TypeRef::Bytes | TypeRef::BorrowedBytes => {
                            w.line(format!("int {n0}_has;"));
                            w.line(format!("uint8_t* {n0};"));
                            w.line(format!("size_t {};", slots[1].name));
                        }
                        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => {
                            w.line(format!("void* {n0};"));
                        }
                        other => {
                            w.line(format!("int {n0}_has;"));
                            w.line(format!(
                                "{} {n0};",
                                abi::element_ctype(other, "").render_c(prefix)
                            ));
                        }
                    },
                    TypeRef::List(inner) => {
                        let elem = elem_payload_ctype(inner, prefix);
                        w.line(format!("{elem}* {n0};"));
                        w.line(format!("size_t {};", slots[1].name));
                    }
                    TypeRef::Map(k, v) => {
                        let kt = elem_payload_ctype(k, prefix);
                        let vt = elem_payload_ctype(v, prefix);
                        w.line(format!("{kt}* {n0};"));
                        w.line(format!("{vt}* {};", slots[1].name));
                        w.line(format!("size_t {};", slots[2].name));
                    }
                    TypeRef::Iterator(_) => {
                        unreachable!("validated: iterator not a callback param")
                    }
                    TypeRef::Interface(_) => {
                        unreachable!("validated: interface not a callback param")
                    }
                    TypeRef::Named(_) => unreachable!("unresolved type reference"),
                }
            }
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The payload element type for list/map callback parameters. Strings own
/// their copies (`char*`); scalar elements keep their C ABI type.
fn elem_payload_ctype(ty: &TypeRef, prefix: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "char*".into(),
        other => abi::element_ctype(other, "").render_c(prefix),
    }
}

/// The producer-thread trampoline: deep-copies the C arguments into a payload
/// and queues it onto the threadsafe function. Runs on whatever thread the
/// producer fires the event from; never touches `napi_env`.
fn render_cb_tramp(out: &mut String, cb: &CallbackBinding, prefix: &str) {
    let payload = cb_payload_name(cb);
    let mut decls = cb_slot_decls(cb, prefix);
    decls.push("void* context".into());
    out.push_str(&format!(
        "static void {}_napi_tramp({}) {{\n",
        cb.c_fn_type,
        decls.join(", ")
    ));
    out.push_str(&format!(
        "    {prefix}_napi_listener_ctx* ctx = ({prefix}_napi_listener_ctx*)context;\n"
    ));
    out.push_str(&format!(
        "    {payload}* p = ({payload}*)calloc(1, sizeof({payload}));\n"
    ));
    for p in &cb.params {
        let slots = abi::lower_param(&p.name, &p.ty, "", false);
        let n0 = &slots[0].name;
        match &p.ty {
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::I64
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_) => {
                out.push_str(&format!("    p->{n0} = {n0};\n"));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("    p->{n0} = {n0} ? strdup({n0}) : NULL;\n"));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let n1 = &slots[1].name;
                out.push_str(&format!("    p->{n1} = {n1};\n"));
                out.push_str(&format!(
                    "    if ({n0} != NULL && {n1} > 0) {{ p->{n0} = (uint8_t*)malloc({n1}); memcpy(p->{n0}, {n0}, {n1}); }}\n"
                ));
            }
            TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => {
                out.push_str(&format!("    p->{n0} = (void*){n0};\n"));
            }
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str(&format!("    p->{n0} = {n0} ? strdup({n0}) : NULL;\n"));
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => {
                    let n1 = &slots[1].name;
                    out.push_str(&format!("    p->{n0}_has = {n0} != NULL;\n"));
                    out.push_str(&format!("    p->{n1} = {n1};\n"));
                    out.push_str(&format!(
                        "    if ({n0} != NULL && {n1} > 0) {{ p->{n0} = (uint8_t*)malloc({n1}); memcpy(p->{n0}, {n0}, {n1}); }}\n"
                    ));
                }
                TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!("    p->{n0} = (void*){n0};\n"));
                }
                _ => {
                    out.push_str(&format!("    p->{n0}_has = {n0} != NULL;\n"));
                    out.push_str(&format!("    if ({n0} != NULL) p->{n0} = *{n0};\n"));
                }
            },
            TypeRef::List(inner) => {
                let n1 = &slots[1].name;
                out.push_str(&format!("    p->{n1} = {n1};\n"));
                match inner.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                        out.push_str(&format!(
                            "    if ({n0} != NULL && {n1} > 0) {{\n        p->{n0} = (char**)calloc({n1}, sizeof(char*));\n        for (size_t i = 0; i < {n1}; i++) p->{n0}[i] = {n0}[i] ? strdup({n0}[i]) : NULL;\n    }}\n"
                        ));
                    }
                    _ => {
                        out.push_str(&format!(
                            "    if ({n0} != NULL && {n1} > 0) {{ p->{n0} = malloc({n1} * sizeof(*p->{n0})); memcpy(p->{n0}, {n0}, {n1} * sizeof(*p->{n0})); }}\n"
                        ));
                    }
                }
            }
            TypeRef::Map(k, v) => {
                let keys = n0;
                let vals = &slots[1].name;
                let len = &slots[2].name;
                out.push_str(&format!("    p->{len} = {len};\n"));
                for (base, ty) in [(keys, k), (vals, v)] {
                    match ty.as_ref() {
                        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                            out.push_str(&format!(
                                "    if ({base} != NULL && {len} > 0) {{\n        p->{base} = (char**)calloc({len}, sizeof(char*));\n        for (size_t i = 0; i < {len}; i++) p->{base}[i] = {base}[i] ? strdup({base}[i]) : NULL;\n    }}\n"
                            ));
                        }
                        _ => {
                            out.push_str(&format!(
                                "    if ({base} != NULL && {len} > 0) {{ p->{base} = malloc({len} * sizeof(*p->{base})); memcpy(p->{base}, {base}, {len} * sizeof(*p->{base})); }}\n"
                            ));
                        }
                    }
                }
            }
            TypeRef::Iterator(_) => unreachable!("validated: iterator not a callback param"),
            TypeRef::Interface(_) => unreachable!("validated: interface not a callback param"),
            TypeRef::Named(_) => unreachable!("unresolved type reference"),
        }
    }
    out.push_str("    napi_call_threadsafe_function(ctx->tsfn, p, napi_tsfn_nonblocking);\n");
    out.push_str("}\n\n");
}

/// One payload field rendered to a `napi_value` in `argv[idx]` (call-js side).
fn emit_payload_to_napi(out: &mut String, p: &ParamBinding, idx: usize, prefix: &str) {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = &slots[0].name;
    let target = format!("argv[{idx}]");
    let _ = prefix;
    match &p.ty {
        TypeRef::I32 => out.push_str(&format!(
            "        napi_create_int32(env, p->{n0}, &{target});\n"
        )),
        TypeRef::U32 => out.push_str(&format!(
            "        napi_create_uint32(env, p->{n0}, &{target});\n"
        )),
        TypeRef::I64 => out.push_str(&format!(
            "        napi_create_int64(env, p->{n0}, &{target});\n"
        )),
        TypeRef::F64 => out.push_str(&format!(
            "        napi_create_double(env, p->{n0}, &{target});\n"
        )),
        TypeRef::I8 | TypeRef::I16 => out.push_str(&format!(
            "        napi_create_int32(env, p->{n0}, &{target});\n"
        )),
        TypeRef::U8 | TypeRef::U16 => out.push_str(&format!(
            "        napi_create_uint32(env, p->{n0}, &{target});\n"
        )),
        TypeRef::U64 => out.push_str(&format!(
            "        napi_create_int64(env, (int64_t)p->{n0}, &{target});\n"
        )),
        TypeRef::F32 => out.push_str(&format!(
            "        napi_create_double(env, p->{n0}, &{target});\n"
        )),
        TypeRef::Bool => out.push_str(&format!(
            "        napi_get_boolean(env, p->{n0}, &{target});\n"
        )),
        TypeRef::Handle => out.push_str(&format!(
            "        napi_create_int64(env, (int64_t)p->{n0}, &{target});\n"
        )),
        TypeRef::Enum(_) => out.push_str(&format!(
            "        napi_create_int32(env, (int32_t)p->{n0}, &{target});\n"
        )),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => out.push_str(&format!(
            "        napi_create_string_utf8(env, p->{n0} ? p->{n0} : \"\", NAPI_AUTO_LENGTH, &{target});\n"
        )),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = &slots[1].name;
            out.push_str(&format!(
                "        napi_create_buffer_copy(env, p->{n1}, p->{n0} ? (const void*)p->{n0} : (const void*)\"\", NULL, &{target});\n"
            ));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => out.push_str(&format!(
            "        napi_create_int64(env, (int64_t)(intptr_t)p->{n0}, &{target});\n"
        )),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => out.push_str(&format!(
                "        if (p->{n0}) napi_create_string_utf8(env, p->{n0}, NAPI_AUTO_LENGTH, &{target}); else napi_get_null(env, &{target});\n"
            )),
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let n1 = &slots[1].name;
                out.push_str(&format!(
                    "        if (p->{n0}_has) napi_create_buffer_copy(env, p->{n1}, p->{n0} ? (const void*)p->{n0} : (const void*)\"\", NULL, &{target}); else napi_get_null(env, &{target});\n"
                ));
            }
            TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => out.push_str(&format!(
                "        if (p->{n0}) napi_create_int64(env, (int64_t)(intptr_t)p->{n0}, &{target}); else napi_get_null(env, &{target});\n"
            )),
            other => {
                let leaf = payload_leaf_to_napi(other, &format!("p->{n0}"), &target);
                out.push_str(&format!(
                    "        if (p->{n0}_has) {{ {leaf} }} else napi_get_null(env, &{target});\n"
                ));
            }
        },
        TypeRef::List(inner) => {
            let n1 = &slots[1].name;
            out.push_str(&format!("        napi_create_array(env, &{target});\n"));
            out.push_str(&format!(
                "        for (size_t i = 0; p->{n0} != NULL && i < p->{n1}; i++) {{\n"
            ));
            out.push_str("            napi_value elem;\n");
            let leaf = payload_elem_to_napi(inner, &format!("p->{n0}[i]"), "elem");
            out.push_str(&format!("            {leaf}\n"));
            out.push_str(&format!(
                "            napi_set_element(env, {target}, (uint32_t)i, elem);\n"
            ));
            out.push_str("        }\n");
        }
        TypeRef::Map(k, v) => {
            let keys = n0;
            let vals = &slots[1].name;
            let len = &slots[2].name;
            out.push_str(&format!("        napi_create_object(env, &{target});\n"));
            out.push_str(&format!(
                "        for (size_t i = 0; p->{keys} != NULL && p->{vals} != NULL && i < p->{len}; i++) {{\n"
            ));
            out.push_str("            napi_value mk; napi_value mv;\n");
            let kc = payload_elem_to_napi(k, &format!("p->{keys}[i]"), "mk");
            let vc = payload_elem_to_napi(v, &format!("p->{vals}[i]"), "mv");
            out.push_str(&format!("            {kc}\n"));
            out.push_str(&format!("            {vc}\n"));
            out.push_str(&format!(
                "            napi_set_property(env, {target}, mk, mv);\n"
            ));
            out.push_str("        }\n");
        }
        TypeRef::Iterator(_) => unreachable!("validated: iterator not a callback param"),
        TypeRef::Interface(_) => unreachable!("validated: interface not a callback param"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// One scalar-ish payload value to a napi_value (single statement).
fn payload_leaf_to_napi(ty: &TypeRef, expr: &str, target: &str) -> String {
    match ty {
        TypeRef::I32 => format!("napi_create_int32(env, {expr}, &{target});"),
        TypeRef::U32 => format!("napi_create_uint32(env, {expr}, &{target});"),
        TypeRef::I64 => format!("napi_create_int64(env, {expr}, &{target});"),
        TypeRef::F64 => format!("napi_create_double(env, {expr}, &{target});"),
        TypeRef::I8 | TypeRef::I16 => format!("napi_create_int32(env, {expr}, &{target});"),
        TypeRef::U8 | TypeRef::U16 => format!("napi_create_uint32(env, {expr}, &{target});"),
        TypeRef::U64 => format!("napi_create_int64(env, (int64_t){expr}, &{target});"),
        TypeRef::F32 => format!("napi_create_double(env, {expr}, &{target});"),
        TypeRef::Bool => format!("napi_get_boolean(env, {expr}, &{target});"),
        TypeRef::Handle => format!("napi_create_int64(env, (int64_t){expr}, &{target});"),
        TypeRef::Enum(_) => format!("napi_create_int32(env, (int32_t){expr}, &{target});"),
        _ => format!("napi_get_null(env, &{target});"),
    }
}

/// One list/map element payload value to a napi_value (single statement).
fn payload_elem_to_napi(ty: &TypeRef, expr: &str, target: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!(
            "napi_create_string_utf8(env, {expr} ? {expr} : \"\", NAPI_AUTO_LENGTH, &{target});"
        ),
        other => payload_leaf_to_napi(other, expr, target),
    }
}

/// Frees one payload field after the JS call.
fn emit_payload_free(out: &mut String, p: &ParamBinding) {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = &slots[0].name;
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("    free(p->{n0});\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("    free(p->{n0});\n"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes => {
                out.push_str(&format!("    free(p->{n0});\n"));
            }
            _ => {}
        },
        TypeRef::List(inner) => {
            let n1 = &slots[1].name;
            if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
                out.push_str(&format!(
                    "    for (size_t i = 0; p->{n0} != NULL && i < p->{n1}; i++) free(p->{n0}[i]);\n"
                ));
            }
            out.push_str(&format!("    free(p->{n0});\n"));
        }
        TypeRef::Map(k, v) => {
            let keys = n0;
            let vals = &slots[1].name;
            let len = &slots[2].name;
            for (base, ty) in [(keys, k), (vals, v)] {
                if matches!(ty.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
                    out.push_str(&format!(
                        "    for (size_t i = 0; p->{base} != NULL && i < p->{len}; i++) free(p->{base}[i]);\n"
                    ));
                }
                out.push_str(&format!("    free(p->{base});\n"));
            }
        }
        _ => {}
    }
}

/// The JS-thread marshaller invoked by the threadsafe function: converts the
/// payload into JS arguments, calls the user callback, and frees the payload.
fn render_cb_calljs(out: &mut String, cb: &CallbackBinding, prefix: &str) {
    let payload = cb_payload_name(cb);
    out.push_str(&format!(
        "static void {}_napi_calljs(napi_env env, napi_value js_cb, void* context, void* data) {{\n",
        cb.c_fn_type
    ));
    out.push_str("    (void)context;\n");
    out.push_str(&format!("    {payload}* p = ({payload}*)data;\n"));
    out.push_str("    if (env != NULL) {\n");
    out.push_str("        napi_value undefined;\n");
    out.push_str("        napi_get_undefined(env, &undefined);\n");
    let argc = cb.params.len();
    if argc > 0 {
        out.push_str(&format!("        napi_value argv[{argc}];\n"));
        for (i, p) in cb.params.iter().enumerate() {
            emit_payload_to_napi(out, p, i, prefix);
        }
        out.push_str(&format!(
            "        napi_call_function(env, undefined, js_cb, {argc}, argv, NULL);\n"
        ));
    } else {
        out.push_str("        napi_call_function(env, undefined, js_cb, 0, NULL, NULL);\n");
    }
    out.push_str("    }\n");
    for p in &cb.params {
        emit_payload_free(out, p);
    }
    out.push_str("    free(p);\n");
    out.push_str("}\n\n");
}

/// The `Napi_*` register/unregister entry points for one listener. Register
/// wraps the JS callback in an unref'd threadsafe function (so live listeners
/// don't pin the event loop) and stores it in the registry; unregister stops
/// the producer first, then releases the threadsafe function.
fn render_listener_napi_fns(
    out: &mut String,
    l: &ListenerBinding,
    cb: &CallbackBinding,
    prefix: &str,
) {
    let register_sym = &l.register_symbol;
    let unregister_sym = &l.unregister_symbol;
    let tramp = format!("{}_napi_tramp", cb.c_fn_type);
    let calljs = format!("{}_napi_calljs", cb.c_fn_type);

    out.push_str(&format!(
        "static napi_value Napi_{register_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str(&format!(
        "  {prefix}_napi_listener_ctx* ctx = ({prefix}_napi_listener_ctx*)calloc(1, sizeof({prefix}_napi_listener_ctx));\n"
    ));
    out.push_str("  napi_value resource_name;\n");
    out.push_str(&format!(
        "  napi_create_string_utf8(env, \"{register_sym}\", NAPI_AUTO_LENGTH, &resource_name);\n"
    ));
    out.push_str(&format!(
        "  napi_create_threadsafe_function(env, args[0], NULL, resource_name, 0, 1, NULL, NULL, NULL, {calljs}, &ctx->tsfn);\n"
    ));
    out.push_str("  napi_unref_threadsafe_function(env, ctx->tsfn);\n");
    out.push_str(&format!("  uint64_t id = {register_sym}({tramp}, ctx);\n"));
    out.push_str("  ctx->id = id;\n");
    out.push_str(&format!("  ctx->next = {prefix}_napi_listeners;\n"));
    out.push_str(&format!("  {prefix}_napi_listeners = ctx;\n"));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_create_double(env, (double)id, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "static napi_value Napi_{unregister_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  double id_d = 0;\n");
    out.push_str("  napi_get_value_double(env, args[0], &id_d);\n");
    out.push_str("  uint64_t id = (uint64_t)id_d;\n");
    // Stop producer-side delivery before tearing down the tsfn so no new
    // payloads are queued against a released function.
    out.push_str(&format!("  {unregister_sym}(id);\n"));
    out.push_str(&format!(
        "  {prefix}_napi_listener_ctx** link = &{prefix}_napi_listeners;\n"
    ));
    out.push_str("  while (*link != NULL) {\n");
    out.push_str("    if ((*link)->id == id) {\n");
    out.push_str(&format!(
        "      {prefix}_napi_listener_ctx* found = *link;\n"
    ));
    out.push_str("      *link = found->next;\n");
    out.push_str("      napi_release_threadsafe_function(found->tsfn, napi_tsfn_release);\n");
    out.push_str("      free(found);\n");
    out.push_str("      break;\n");
    out.push_str("    }\n");
    out.push_str("    link = &(*link)->next;\n");
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

fn async_cb_result_params_node(ret: Option<&TypeRef>, module: &str, prefix: &str) -> String {
    match ret {
        None => String::new(),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => ", const char* result".into(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            ", const uint8_t* result, size_t result_len".into()
        }
        Some(TypeRef::List(inner)) => {
            let et = c_elem_type(inner, module, prefix);
            format!(", {et}* result, size_t result_len")
        }
        Some(TypeRef::Map(k, v)) => {
            let kt = c_elem_type(k, module, prefix);
            let vt = c_elem_type(v, module, prefix);
            format!(", {kt}* result_keys, {vt}* result_values, size_t result_len")
        }
        Some(t) => format!(", {} result", c_ret_type_str(t, module, prefix)),
    }
}

/// Emit the per-async-function machinery: a context struct carrying the
/// promise + threadsafe function + deep-copied results, the producer-thread
/// completion callback (which only copies and queues), and the JS-thread
/// marshaller (which settles the promise).
///
/// The completion callback may fire on any thread, so it must never touch
/// `napi_env`; the ref'd threadsafe function also keeps the event loop alive
/// until the promise settles.
fn render_async_machinery(
    out: &mut String,
    f: &FnBinding,
    c_name: &str,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
) {
    let actx = format!("{c_name}_napi_actx");
    let cb_name = format!("{c_name}_napi_cb");
    let calljs = format!("{c_name}_napi_settle");
    let cb_result = async_cb_result_params_node(f.ret.as_ref(), module, prefix);

    // -- context struct --
    out.push_str("typedef struct {\n");
    out.push_str("    napi_deferred deferred;\n");
    out.push_str("    napi_threadsafe_function tsfn;\n");
    out.push_str("    int32_t err_code;\n");
    out.push_str("    char* err_msg;\n");
    match f.ret.as_ref() {
        None => {}
        Some(TypeRef::I32) => out.push_str("    int32_t result;\n"),
        Some(TypeRef::U32) => out.push_str("    uint32_t result;\n"),
        Some(TypeRef::I64) => out.push_str("    int64_t result;\n"),
        Some(TypeRef::F64) => out.push_str("    double result;\n"),
        Some(TypeRef::I8) => out.push_str("    int8_t result;\n"),
        Some(TypeRef::I16) => out.push_str("    int16_t result;\n"),
        Some(TypeRef::U8) => out.push_str("    uint8_t result;\n"),
        Some(TypeRef::U16) => out.push_str("    uint16_t result;\n"),
        Some(TypeRef::U64) => out.push_str("    uint64_t result;\n"),
        Some(TypeRef::F32) => out.push_str("    float result;\n"),
        Some(TypeRef::Bool) => out.push_str("    bool result;\n"),
        Some(TypeRef::Enum(_)) => out.push_str("    int32_t result;\n"),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str("    char* result;\n");
            out.push_str("    int result_null;\n");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str("    uint8_t* result;\n");
            out.push_str("    size_t result_len;\n");
        }
        Some(TypeRef::Handle) => out.push_str("    uint64_t result;\n"),
        Some(
            TypeRef::TypedHandle(_)
            | TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
            | TypeRef::Iterator(_),
        ) => out.push_str("    void* result;\n"),
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("    char* result;\n");
                out.push_str("    int result_null;\n");
            }
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
            | TypeRef::TypedHandle(_) => {
                out.push_str("    void* result;\n");
            }
            other => {
                out.push_str("    int result_has;\n");
                out.push_str(&format!(
                    "    {} result;\n",
                    c_elem_type(other, module, prefix)
                ));
            }
        },
        Some(TypeRef::List(inner)) => {
            let elem = match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "char*".to_string(),
                other => c_elem_type(other, module, prefix),
            };
            out.push_str(&format!("    {elem}* result;\n"));
            out.push_str("    size_t result_len;\n");
        }
        Some(TypeRef::Map(k, v)) => {
            for (field, ty) in [("result_keys", k), ("result_values", v)] {
                let elem = match ty.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => "char*".to_string(),
                    other => c_elem_type(other, module, prefix),
                };
                out.push_str(&format!("    {elem}* {field};\n"));
            }
            out.push_str("    size_t result_len;\n");
        }
        Some(TypeRef::Named(_)) => unreachable!("unresolved type reference"),
    }
    out.push_str(&format!("}} {actx};\n\n"));

    // -- producer-thread completion callback: deep-copy + queue --
    out.push_str(&format!(
        "static void {cb_name}(void* context, weaveffi_error* err{cb_result}) {{\n"
    ));
    out.push_str(&format!("    {actx}* ctx = ({actx}*)context;\n"));
    out.push_str("    if (err != NULL && err->code != 0) {\n");
    out.push_str("        ctx->err_code = err->code;\n");
    out.push_str(
        "        ctx->err_msg = err->message ? strdup(err->message) : strdup(\"unknown error\");\n",
    );
    out.push_str("    } else {\n");
    match f.ret.as_ref() {
        None => {}
        Some(
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::I64
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle,
        ) => {
            out.push_str("        ctx->result = result;\n");
        }
        Some(TypeRef::Enum(_)) => {
            out.push_str("        ctx->result = (int32_t)result;\n");
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str("        ctx->result_null = result == NULL;\n");
            out.push_str("        ctx->result = result ? strdup(result) : NULL;\n");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result, result_len); }\n",
            );
        }
        // Owned-object results (records, rich enums, interfaces, handles,
        // iterators) are adopted by the receiver, so the pointer stays valid
        // across the thread hop; everything borrowed is deep-copied above.
        Some(
            TypeRef::TypedHandle(_)
            | TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
            | TypeRef::Iterator(_),
        ) => {
            out.push_str("        ctx->result = (void*)result;\n");
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("        ctx->result_null = result == NULL;\n");
                out.push_str("        ctx->result = result ? strdup(result) : NULL;\n");
            }
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
            | TypeRef::TypedHandle(_) => {
                out.push_str("        ctx->result = (void*)result;\n");
            }
            _ => {
                out.push_str("        ctx->result_has = result != NULL;\n");
                out.push_str("        if (result != NULL) ctx->result = *result;\n");
            }
        },
        Some(TypeRef::List(inner)) => {
            out.push_str("        ctx->result_len = result_len;\n");
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str(
                        "        if (result != NULL && result_len > 0) {\n            ctx->result = (char**)calloc(result_len, sizeof(char*));\n            for (size_t i = 0; i < result_len; i++) ctx->result[i] = result[i] ? strdup(result[i]) : NULL;\n        }\n",
                    );
                }
                _ => {
                    out.push_str(
                        "        if (result != NULL && result_len > 0) { ctx->result = malloc(result_len * sizeof(*ctx->result)); memcpy(ctx->result, result, result_len * sizeof(*ctx->result)); }\n",
                    );
                }
            }
        }
        Some(TypeRef::Map(k, v)) => {
            out.push_str("        ctx->result_len = result_len;\n");
            for (field, src, ty) in [
                ("result_keys", "result_keys", k),
                ("result_values", "result_values", v),
            ] {
                match ty.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                        out.push_str(&format!(
                            "        if ({src} != NULL && result_len > 0) {{\n            ctx->{field} = (char**)calloc(result_len, sizeof(char*));\n            for (size_t i = 0; i < result_len; i++) ctx->{field}[i] = {src}[i] ? strdup({src}[i]) : NULL;\n        }}\n"
                        ));
                    }
                    _ => {
                        out.push_str(&format!(
                            "        if ({src} != NULL && result_len > 0) {{ ctx->{field} = malloc(result_len * sizeof(*ctx->{field})); memcpy(ctx->{field}, {src}, result_len * sizeof(*ctx->{field})); }}\n"
                        ));
                    }
                }
            }
        }
        Some(TypeRef::Named(_)) => unreachable!("unresolved type reference"),
    }
    out.push_str("    }\n");
    out.push_str("    napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking);\n");
    out.push_str("}\n\n");

    // -- JS-thread marshaller: settle the promise, free, release --
    out.push_str(&format!(
        "static void {calljs}(napi_env env, napi_value js_cb, void* context, void* data) {{\n"
    ));
    out.push_str("    (void)js_cb;\n");
    out.push_str("    (void)context;\n");
    out.push_str(&format!("    {actx}* ctx = ({actx}*)data;\n"));
    out.push_str("    if (env != NULL) {\n");
    out.push_str("    if (ctx->err_code != 0) {\n");
    out.push_str(&format!(
        "        napi_value err_obj = {prefix}_napi_error_value(env, ctx->err_code, ctx->err_msg);\n"
    ));
    out.push_str("        napi_reject_deferred(env, ctx->deferred, err_obj);\n");
    out.push_str("    } else {\n");
    out.push_str("        napi_value val;\n");
    match f.ret.as_ref() {
        None => out.push_str("        napi_get_undefined(env, &val);\n"),
        Some(TypeRef::I32) => out.push_str("        napi_create_int32(env, ctx->result, &val);\n"),
        Some(TypeRef::U32) => out.push_str("        napi_create_uint32(env, ctx->result, &val);\n"),
        Some(TypeRef::I64) => out.push_str("        napi_create_int64(env, ctx->result, &val);\n"),
        Some(TypeRef::F64) => out.push_str("        napi_create_double(env, ctx->result, &val);\n"),
        Some(TypeRef::I8 | TypeRef::I16) => {
            out.push_str("        napi_create_int32(env, ctx->result, &val);\n")
        }
        Some(TypeRef::U8 | TypeRef::U16) => {
            out.push_str("        napi_create_uint32(env, ctx->result, &val);\n")
        }
        Some(TypeRef::U64) => {
            out.push_str("        napi_create_int64(env, (int64_t)ctx->result, &val);\n")
        }
        Some(TypeRef::F32) => out.push_str("        napi_create_double(env, ctx->result, &val);\n"),
        Some(TypeRef::Bool) => out.push_str("        napi_get_boolean(env, ctx->result, &val);\n"),
        Some(TypeRef::Enum(_)) => {
            out.push_str("        napi_create_int32(env, ctx->result, &val);\n");
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str(
                "        if (ctx->result_null) napi_get_null(env, &val); else napi_create_string_utf8(env, ctx->result ? ctx->result : \"\", NAPI_AUTO_LENGTH, &val);\n",
            );
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(
                "        napi_create_buffer_copy(env, ctx->result_len, ctx->result ? (const void*)ctx->result : (const void*)\"\", NULL, &val);\n",
            );
        }
        Some(TypeRef::Handle) => {
            out.push_str("        napi_create_int64(env, (int64_t)ctx->result, &val);\n");
        }
        // An interface or rich-enum result stays the raw owned pointer here;
        // the JS loader wraps the settled handle in its class, which owns
        // disposal.
        Some(
            TypeRef::TypedHandle(_)
            | TypeRef::Interface(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Iterator(_),
        ) => {
            out.push_str("        napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n");
        }
        Some(TypeRef::Record(name)) => {
            emit_struct_to_object(
                out,
                "env",
                name,
                "ctx->result",
                "val",
                module,
                prefix,
                structs,
                "        ",
                true,
            );
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(
                    "        if (ctx->result_null) napi_get_null(env, &val); else napi_create_string_utf8(env, ctx->result ? ctx->result : \"\", NAPI_AUTO_LENGTH, &val);\n",
                );
            }
            TypeRef::Record(name) => {
                out.push_str(
                    "        if (ctx->result == NULL) { napi_get_null(env, &val); } else {\n",
                );
                emit_struct_to_object(
                    out,
                    "env",
                    name,
                    "ctx->result",
                    "val",
                    module,
                    prefix,
                    structs,
                    "            ",
                    true,
                );
                out.push_str("        }\n");
            }
            TypeRef::TypedHandle(_) | TypeRef::Interface(_) | TypeRef::RichEnum(_) => {
                out.push_str(
                    "        if (ctx->result == NULL) napi_get_null(env, &val); else napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n",
                );
            }
            other => {
                let leaf = payload_leaf_to_napi(other, "ctx->result", "val");
                out.push_str(&format!(
                    "        if (ctx->result_has) {{ {leaf} }} else napi_get_null(env, &val);\n"
                ));
            }
        },
        Some(TypeRef::List(inner)) => {
            out.push_str("        napi_create_array(env, &val);\n");
            out.push_str(
                "        for (size_t i = 0; ctx->result != NULL && i < ctx->result_len; i++) {\n",
            );
            out.push_str("            napi_value elem;\n");
            let leaf = payload_elem_to_napi(inner, "ctx->result[i]", "elem");
            out.push_str(&format!("            {leaf}\n"));
            out.push_str("            napi_set_element(env, val, (uint32_t)i, elem);\n");
            out.push_str("        }\n");
        }
        Some(TypeRef::Map(k, v)) => {
            out.push_str("        napi_create_object(env, &val);\n");
            out.push_str(
                "        for (size_t i = 0; ctx->result_keys != NULL && ctx->result_values != NULL && i < ctx->result_len; i++) {\n",
            );
            out.push_str("            napi_value mk; napi_value mv;\n");
            let kc = payload_elem_to_napi(k, "ctx->result_keys[i]", "mk");
            let vc = payload_elem_to_napi(v, "ctx->result_values[i]", "mv");
            out.push_str(&format!("            {kc}\n"));
            out.push_str(&format!("            {vc}\n"));
            out.push_str("            napi_set_property(env, val, mk, mv);\n");
            out.push_str("        }\n");
        }
        Some(TypeRef::Named(_)) => unreachable!("unresolved type reference"),
    }
    out.push_str("        napi_resolve_deferred(env, ctx->deferred, val);\n");
    out.push_str("    }\n");
    out.push_str("    }\n");
    out.push_str("    free(ctx->err_msg);\n");
    match f.ret.as_ref() {
        Some(
            TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes,
        ) => {
            out.push_str("    free(ctx->result);\n");
        }
        Some(TypeRef::Optional(inner))
            if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            out.push_str("    free(ctx->result);\n");
        }
        Some(TypeRef::List(inner)) => {
            if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
                out.push_str(
                    "    for (size_t i = 0; ctx->result != NULL && i < ctx->result_len; i++) free(ctx->result[i]);\n",
                );
            }
            out.push_str("    free(ctx->result);\n");
        }
        Some(TypeRef::Map(k, v)) => {
            for (field, ty) in [("result_keys", k), ("result_values", v)] {
                if matches!(ty.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
                    out.push_str(&format!(
                        "    for (size_t i = 0; ctx->{field} != NULL && i < ctx->result_len; i++) free(ctx->{field}[i]);\n"
                    ));
                }
                out.push_str(&format!("    free(ctx->{field});\n"));
            }
        }
        _ => {}
    }
    out.push_str("    napi_release_threadsafe_function(ctx->tsfn, napi_tsfn_release);\n");
    out.push_str("    free(ctx);\n");
    out.push_str("}\n\n");
}

/// Read the wrapped interface pointer from `args[0]` and push it as the
/// leading C argument. Instance methods carry this implicit `self` slot in
/// their [`AbiFn`](weaveffi_core::model::AbiFn) signatures; the JS class
/// passes its own handle there.
fn emit_self_arg(out: &mut String, c_args: &mut Vec<String>, self_tag: &str) {
    out.push_str("  int64_t self_raw;\n");
    out.push_str("  napi_get_value_int64(env, args[0], &self_raw);\n");
    c_args.push(format!("(const {self_tag}*)(intptr_t)self_raw"));
}

/// Read `argc`/`args` for a callable with `n` incoming JS arguments
/// (including the leading handle of an instance method).
fn emit_args_read(out: &mut String, n: usize) {
    if n > 0 {
        out.push_str(&format!("  size_t argc = {n};\n"));
        out.push_str(&format!("  napi_value args[{n}];\n"));
        out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    } else {
        out.push_str("  size_t argc = 0;\n");
        out.push_str("  napi_get_cb_info(env, info, &argc, NULL, NULL, NULL);\n");
    }
}

fn render_async_napi_body(
    out: &mut String,
    f: &FnBinding,
    module: &str,
    prefix: &str,
    self_tag: Option<&str>,
) {
    let c_name = &f.c_base;
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("async body rendered for a non-async callable");
    };
    let offset = usize::from(self_tag.is_some());
    emit_args_read(out, f.params.len() + offset);

    let mut c_args: Vec<String> = Vec::new();
    let mut cleanups: Vec<String> = Vec::new();
    if let Some(tag) = self_tag {
        emit_self_arg(out, &mut c_args, tag);
    }
    for (i, p) in f.params.iter().enumerate() {
        emit_param(
            out,
            &mut c_args,
            &mut cleanups,
            &p.ty,
            &p.name,
            i + offset,
            module,
            prefix,
        );
    }

    let actx = format!("{c_name}_napi_actx");
    out.push_str(&format!(
        "  {actx}* ctx = ({actx}*)calloc(1, sizeof({actx}));\n"
    ));
    out.push_str("  napi_value promise;\n");
    out.push_str("  napi_create_promise(env, &ctx->deferred, &promise);\n");
    out.push_str("  napi_value resource_name;\n");
    out.push_str(&format!(
        "  napi_create_string_utf8(env, \"{c_name}\", NAPI_AUTO_LENGTH, &resource_name);\n"
    ));
    // Ref'd (unlike listeners): a pending promise must keep the loop alive.
    out.push_str(&format!(
        "  napi_create_threadsafe_function(env, NULL, NULL, resource_name, 0, 1, NULL, NULL, NULL, {c_name}_napi_settle, &ctx->tsfn);\n"
    ));

    if f.cancellable {
        c_args.push("NULL".into());
    }

    let cb_name = format!("{c_name}_napi_cb");
    c_args.push(cb_name);
    c_args.push("ctx".into());
    let args_str = c_args.join(", ");
    out.push_str(&format!("  {}({args_str});\n", ab.launch.symbol));

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    out.push_str("  return promise;\n");
}

fn render_napi_body(
    out: &mut String,
    f: &FnBinding,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
    self_tag: Option<&str>,
) {
    // The launcher symbol comes from the lowered shape rather than being
    // rebuilt from the name, so interface members call the right entry point.
    let symbol = match &f.shape {
        CallShape::Sync(abi) => &abi.symbol,
        CallShape::Iterator(ib) => &ib.launch.symbol,
        CallShape::Async(_) => unreachable!("sync body rendered for an async callable"),
    };
    let offset = usize::from(self_tag.is_some());
    emit_args_read(out, f.params.len() + offset);

    let mut c_args: Vec<String> = Vec::new();
    let mut cleanups: Vec<String> = Vec::new();
    if let Some(tag) = self_tag {
        emit_self_arg(out, &mut c_args, tag);
    }
    for (i, p) in f.params.iter().enumerate() {
        emit_param(
            out,
            &mut c_args,
            &mut cleanups,
            &p.ty,
            &p.name,
            i + offset,
            module,
            prefix,
        );
    }

    out.push_str("  weaveffi_error err = {0};\n");

    if let Some(ret) = &f.ret {
        emit_ret_out_params(out, &mut c_args, ret, module, prefix);
    }
    c_args.push("&err".to_string());

    let args_str = c_args.join(", ");
    let ret_type = f.ret.as_ref().map(|r| c_ret_type_str(r, module, prefix));
    match &ret_type {
        Some(rt) if rt != "void" => {
            out.push_str(&format!("  {rt} result = {symbol}({args_str});\n"));
        }
        _ => {
            out.push_str(&format!("  {symbol}({args_str});\n"));
        }
    }

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    emit_error_check_c(out, prefix);

    match &f.ret {
        Some(ret) => emit_ret_to_napi(out, ret, module, prefix, f, structs),
        None => {
            out.push_str("  napi_value ret;\n");
            out.push_str("  napi_get_undefined(env, &ret);\n");
            out.push_str("  return ret;\n");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    ty: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
    prefix: &str,
) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            let ct = c_elem_type(ty, module, prefix);
            let getter = napi_getter(ty);
            out.push_str(&format!("  {ct} {name};\n"));
            out.push_str(&format!("  {getter}(env, args[{idx}], &{name});\n"));
            c_args.push(name.into());
        }
        // N-API has no narrower-than-32-bit / float getter, so read into a
        // correctly-sized temporary and narrow to the real ABI type.
        TypeRef::I8 | TypeRef::I16 | TypeRef::U8 | TypeRef::U16 | TypeRef::U64 | TypeRef::F32 => {
            let ct = c_elem_type(ty, module, prefix);
            let getter = napi_getter(ty);
            let raw = napi_read_tmp_type(ty);
            out.push_str(&format!("  {raw} {name}_raw;\n"));
            out.push_str(&format!("  {getter}(env, args[{idx}], &{name}_raw);\n"));
            c_args.push(format!("({ct}){name}_raw"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!(
                "  char* {name} = (char*)malloc({name}_len + 1);\n"
            ));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            c_args.push(name.into());
            cleanups.push(format!("  free({name});\n"));
        }
        TypeRef::Handle => {
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("(weaveffi_handle_t){name}_raw"));
        }
        TypeRef::TypedHandle(s) => {
            let abi = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("({abi}*)(intptr_t){name}_raw"));
        }
        TypeRef::Enum(e) => {
            out.push_str(&format!("  int32_t {name};\n"));
            out.push_str(&format!(
                "  napi_get_value_int32(env, args[{idx}], &{name});\n"
            ));
            c_args.push(format!("({prefix}_{module}_{e}){name}"));
        }
        // Records, rich enums, and interfaces arrive as int64 handles wrapping
        // the opaque pointer; rich enums and interfaces additionally get
        // unwrapped from their JS class by the loader before reaching the
        // addon (borrow: pointer only).
        TypeRef::Record(s) | TypeRef::RichEnum(s) | TypeRef::Interface(s) => {
            let abi = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("(const {abi}*)(intptr_t){name}_raw"));
        }
        TypeRef::Optional(inner) => {
            out.push_str(&format!("  napi_valuetype {name}_type;\n"));
            out.push_str(&format!("  napi_typeof(env, args[{idx}], &{name}_type);\n"));
            emit_optional_param(out, c_args, cleanups, inner, name, idx, module, prefix);
        }
        TypeRef::List(inner) => {
            emit_list_param(out, c_args, cleanups, inner, name, idx, module, prefix);
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("  void* {name}_raw;\n"));
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_buffer_info(env, args[{idx}], &{name}_raw, &{name}_len);\n"
            ));
            c_args.push(format!("(const uint8_t*){name}_raw"));
            c_args.push(format!("{name}_len"));
        }
        TypeRef::Map(k, v) => {
            emit_map_param(out, c_args, cleanups, k, v, name, idx, module, prefix);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn emit_opt_val(
    out: &mut String,
    c_args: &mut Vec<String>,
    c_type: &str,
    napi_fn: &str,
    name: &str,
    idx: usize,
) {
    out.push_str(&format!("  {c_type} {name}_val;\n"));
    out.push_str(&format!("  const {c_type}* {name}_ptr = NULL;\n"));
    out.push_str(&format!(
        "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
    ));
    out.push_str(&format!("    {napi_fn}(env, args[{idx}], &{name}_val);\n"));
    out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
    out.push_str("  }\n");
    c_args.push(format!("{name}_ptr"));
}

#[allow(clippy::too_many_arguments)]
fn emit_optional_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    inner: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
    prefix: &str,
) {
    match inner {
        TypeRef::I32 => {
            emit_opt_val(out, c_args, "int32_t", "napi_get_value_int32", name, idx);
        }
        TypeRef::U32 => {
            emit_opt_val(out, c_args, "uint32_t", "napi_get_value_uint32", name, idx);
        }
        TypeRef::I64 => {
            emit_opt_val(out, c_args, "int64_t", "napi_get_value_int64", name, idx);
        }
        TypeRef::F64 => {
            emit_opt_val(out, c_args, "double", "napi_get_value_double", name, idx);
        }
        TypeRef::Bool => {
            emit_opt_val(out, c_args, "bool", "napi_get_value_bool", name, idx);
        }
        TypeRef::Handle => {
            out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
            out.push_str(&format!("  weaveffi_handle_t {name}_val;\n"));
            out.push_str(&format!("  const weaveffi_handle_t* {name}_ptr = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str(&format!(
                "    {name}_val = (weaveffi_handle_t){name}_raw;\n"
            ));
            out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
            out.push_str("  }\n");
            c_args.push(format!("{name}_ptr"));
        }
        // A typed handle is a nullable opaque pointer, so an optional one maps to
        // the same pointer with NULL standing in for absence, mirroring structs.
        TypeRef::TypedHandle(s) => {
            let abi = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str("  }\n");
            c_args.push(format!("{name}_raw ? ({abi}*)(intptr_t){name}_raw : NULL"));
        }
        TypeRef::Enum(e) => {
            let etype = format!("{prefix}_{module}_{e}");
            out.push_str(&format!("  int32_t {name}_raw;\n"));
            out.push_str(&format!("  {etype} {name}_val;\n"));
            out.push_str(&format!("  const {etype}* {name}_ptr = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int32(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str(&format!("    {name}_val = ({etype}){name}_raw;\n"));
            out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
            out.push_str("  }\n");
            c_args.push(format!("{name}_ptr"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("  char* {name} = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!("    size_t {name}_len;\n"));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!("    {name} = (char*)malloc({name}_len + 1);\n"));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            out.push_str("  }\n");
            c_args.push(name.into());
            cleanups.push(format!("  free({name});\n"));
        }
        // An optional record, rich enum, or interface is a nullable opaque
        // pointer: JS null/undefined passes NULL, anything else the wrapped
        // handle.
        TypeRef::Record(s) | TypeRef::RichEnum(s) | TypeRef::Interface(s) => {
            let abi = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str("  }\n");
            c_args.push(format!(
                "{name}_raw ? (const {abi}*)(intptr_t){name}_raw : NULL"
            ));
        }
        // Optional narrow numerics: read through a wider N-API getter into a
        // temporary, narrow to the ABI type, then pass a pointer (NULL absent).
        TypeRef::I8 | TypeRef::I16 | TypeRef::U8 | TypeRef::U16 | TypeRef::U64 | TypeRef::F32 => {
            let ct = c_elem_type(inner, module, prefix);
            let getter = napi_getter(inner);
            let raw = napi_read_tmp_type(inner);
            out.push_str(&format!("  {ct} {name}_val;\n"));
            out.push_str(&format!("  const {ct}* {name}_ptr = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!("    {raw} {name}_raw;\n"));
            out.push_str(&format!("    {getter}(env, args[{idx}], &{name}_raw);\n"));
            out.push_str(&format!("    {name}_val = ({ct}){name}_raw;\n"));
            out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
            out.push_str("  }\n");
            c_args.push(format!("{name}_ptr"));
        }
        _ => {
            emit_param(out, c_args, cleanups, inner, name, idx, module, prefix);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_list_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    inner: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
    prefix: &str,
) {
    let et = c_elem_type(inner, module, prefix);
    out.push_str(&format!("  uint32_t {name}_count;\n"));
    out.push_str(&format!(
        "  napi_get_array_length(env, args[{idx}], &{name}_count);\n"
    ));
    out.push_str(&format!(
        "  {et}* {name}_arr = ({et}*)malloc(sizeof({et}) * ({name}_count + 1));\n"
    ));
    out.push_str(&format!(
        "  for (uint32_t {name}_i = 0; {name}_i < {name}_count; {name}_i++) {{\n"
    ));
    out.push_str(&format!("    napi_value {name}_el;\n"));
    out.push_str(&format!(
        "    napi_get_element(env, args[{idx}], {name}_i, &{name}_el);\n"
    ));

    match inner {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            let getter = napi_getter(inner);
            out.push_str(&format!(
                "    {getter}(env, {name}_el, &{name}_arr[{name}_i]);\n"
            ));
        }
        // Narrow numerics need a wider read temporary, then a narrowing cast
        // into the element slot so the 1/2/8-byte element is not overrun.
        TypeRef::I8 | TypeRef::I16 | TypeRef::U8 | TypeRef::U16 | TypeRef::U64 | TypeRef::F32 => {
            let getter = napi_getter(inner);
            let raw = napi_read_tmp_type(inner);
            out.push_str(&format!("    {raw} {name}_nv;\n"));
            out.push_str(&format!("    {getter}(env, {name}_el, &{name}_nv);\n"));
            out.push_str(&format!("    {name}_arr[{name}_i] = ({et}){name}_nv;\n"));
        }
        TypeRef::Handle => {
            out.push_str(&format!("    int64_t {name}_h;\n"));
            out.push_str(&format!(
                "    napi_get_value_int64(env, {name}_el, &{name}_h);\n"
            ));
            out.push_str(&format!(
                "    {name}_arr[{name}_i] = (weaveffi_handle_t){name}_h;\n"
            ));
        }
        TypeRef::TypedHandle(s) => {
            let abi = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("    int64_t {name}_h;\n"));
            out.push_str(&format!(
                "    napi_get_value_int64(env, {name}_el, &{name}_h);\n"
            ));
            out.push_str(&format!(
                "    {name}_arr[{name}_i] = ({abi}*)(intptr_t){name}_h;\n"
            ));
        }
        TypeRef::Enum(_) => {
            out.push_str(&format!("    int32_t {name}_ev;\n"));
            out.push_str(&format!(
                "    napi_get_value_int32(env, {name}_el, &{name}_ev);\n"
            ));
            out.push_str(&format!("    {name}_arr[{name}_i] = ({et}){name}_ev;\n"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("    size_t {name}_sl;\n"));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, {name}_el, NULL, 0, &{name}_sl);\n"
            ));
            out.push_str(&format!(
                "    char* {name}_s = (char*)malloc({name}_sl + 1);\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, {name}_el, {name}_s, {name}_sl + 1, &{name}_sl);\n"
            ));
            out.push_str(&format!("    {name}_arr[{name}_i] = {name}_s;\n"));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) => {
            out.push_str(&format!("    int64_t {name}_sp;\n"));
            out.push_str(&format!(
                "    napi_get_value_int64(env, {name}_el, &{name}_sp);\n"
            ));
            out.push_str(&format!(
                "    {name}_arr[{name}_i] = ({et})(intptr_t){name}_sp;\n"
            ));
        }
        _ => {
            let getter = napi_getter(inner);
            out.push_str(&format!(
                "    {getter}(env, {name}_el, &{name}_arr[{name}_i]);\n"
            ));
        }
    }

    out.push_str("  }\n");
    c_args.push(format!("{name}_arr"));
    c_args.push(format!("(size_t){name}_count"));

    if matches!(inner, TypeRef::StringUtf8) {
        cleanups.push(format!(
            "  for (uint32_t {name}_j = 0; {name}_j < {name}_count; {name}_j++) free((void*){name}_arr[{name}_j]);\n"
        ));
    }
    cleanups.push(format!("  free({name}_arr);\n"));
}

#[allow(clippy::too_many_arguments)]
fn emit_map_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    k: &TypeRef,
    v: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
    prefix: &str,
) {
    let kt = c_elem_type(k, module, prefix);
    let vt = c_elem_type(v, module, prefix);
    out.push_str(&format!("  napi_value {name}_keys_napi;\n"));
    out.push_str(&format!(
        "  napi_get_property_names(env, args[{idx}], &{name}_keys_napi);\n"
    ));
    out.push_str(&format!("  uint32_t {name}_count;\n"));
    out.push_str(&format!(
        "  napi_get_array_length(env, {name}_keys_napi, &{name}_count);\n"
    ));
    out.push_str(&format!(
        "  {kt}* {name}_keys = ({kt}*)malloc(sizeof({kt}) * ({name}_count + 1));\n"
    ));
    out.push_str(&format!(
        "  {vt}* {name}_values = ({vt}*)malloc(sizeof({vt}) * ({name}_count + 1));\n"
    ));
    out.push_str(&format!(
        "  for (uint32_t {name}_i = 0; {name}_i < {name}_count; {name}_i++) {{\n"
    ));
    out.push_str(&format!("    napi_value {name}_k;\n"));
    out.push_str(&format!(
        "    napi_get_element(env, {name}_keys_napi, {name}_i, &{name}_k);\n"
    ));

    if matches!(k, TypeRef::StringUtf8) {
        out.push_str(&format!("    size_t {name}_kl;\n"));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_k, NULL, 0, &{name}_kl);\n"
        ));
        out.push_str(&format!(
            "    char* {name}_ks = (char*)malloc({name}_kl + 1);\n"
        ));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_k, {name}_ks, {name}_kl + 1, &{name}_kl);\n"
        ));
        out.push_str(&format!("    {name}_keys[{name}_i] = {name}_ks;\n"));
    } else if needs_narrowing_read(k) {
        out.push_str(&format!("    napi_value {name}_kn;\n"));
        out.push_str(&format!(
            "    napi_coerce_to_number(env, {name}_k, &{name}_kn);\n"
        ));
        let kgetter = napi_getter(k);
        let raw = napi_read_tmp_type(k);
        out.push_str(&format!("    {raw} {name}_kv;\n"));
        out.push_str(&format!("    {kgetter}(env, {name}_kn, &{name}_kv);\n"));
        out.push_str(&format!("    {name}_keys[{name}_i] = ({kt}){name}_kv;\n"));
    } else {
        out.push_str(&format!("    napi_value {name}_kn;\n"));
        out.push_str(&format!(
            "    napi_coerce_to_number(env, {name}_k, &{name}_kn);\n"
        ));
        let kgetter = napi_getter(k);
        out.push_str(&format!(
            "    {kgetter}(env, {name}_kn, &{name}_keys[{name}_i]);\n"
        ));
    }

    out.push_str(&format!("    napi_value {name}_v;\n"));
    out.push_str(&format!(
        "    napi_get_property(env, args[{idx}], {name}_k, &{name}_v);\n"
    ));

    if matches!(v, TypeRef::StringUtf8) {
        out.push_str(&format!("    size_t {name}_vl;\n"));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_v, NULL, 0, &{name}_vl);\n"
        ));
        out.push_str(&format!(
            "    char* {name}_vs = (char*)malloc({name}_vl + 1);\n"
        ));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_v, {name}_vs, {name}_vl + 1, &{name}_vl);\n"
        ));
        out.push_str(&format!("    {name}_values[{name}_i] = {name}_vs;\n"));
    } else if needs_narrowing_read(v) {
        let vgetter = napi_getter(v);
        let raw = napi_read_tmp_type(v);
        out.push_str(&format!("    {raw} {name}_vv;\n"));
        out.push_str(&format!("    {vgetter}(env, {name}_v, &{name}_vv);\n"));
        out.push_str(&format!("    {name}_values[{name}_i] = ({vt}){name}_vv;\n"));
    } else {
        let vgetter = napi_getter(v);
        out.push_str(&format!(
            "    {vgetter}(env, {name}_v, &{name}_values[{name}_i]);\n"
        ));
    }

    out.push_str("  }\n");
    c_args.push(format!("{name}_keys"));
    c_args.push(format!("{name}_values"));
    c_args.push(format!("(size_t){name}_count"));

    if matches!(k, TypeRef::StringUtf8) {
        cleanups.push(format!(
            "  for (uint32_t {name}_j = 0; {name}_j < {name}_count; {name}_j++) free((void*){name}_keys[{name}_j]);\n"
        ));
    }
    cleanups.push(format!("  free({name}_keys);\n"));
    if matches!(v, TypeRef::StringUtf8) {
        cleanups.push(format!(
            "  for (uint32_t {name}_j = 0; {name}_j < {name}_count; {name}_j++) free((void*){name}_values[{name}_j]);\n"
        ));
    }
    cleanups.push(format!("  free({name}_values);\n"));
}

fn emit_ret_out_params(
    out: &mut String,
    c_args: &mut Vec<String>,
    ty: &TypeRef,
    module: &str,
    prefix: &str,
) {
    match ty {
        TypeRef::Bytes | TypeRef::List(_) => {
            out.push_str("  size_t out_len;\n");
            c_args.push("&out_len".into());
        }
        TypeRef::Map(k, v) => {
            // The C ABI hands back the base of each producer-allocated array,
            // so the out-params are pointers to the array pointers.
            let kt = c_elem_type(k, module, prefix);
            let vt = c_elem_type(v, module, prefix);
            out.push_str(&format!("  {kt}* out_keys = NULL;\n"));
            out.push_str(&format!("  {vt}* out_values = NULL;\n"));
            out.push_str("  size_t out_len = 0;\n");
            c_args.push("&out_keys".into());
            c_args.push("&out_values".into());
            c_args.push("&out_len".into());
        }
        TypeRef::Optional(inner) if is_c_pointer_type(inner) => {
            emit_ret_out_params(out, c_args, inner, module, prefix);
        }
        _ => {}
    }
}

/// Build a `name -> StructDef` registry over every (possibly nested) module so
/// that struct-returning functions can materialize a real JS object (matching
/// the shape declared in `types.d.ts`) instead of leaking a raw handle number.
fn struct_registry(model: &BindingModel) -> HashMap<String, StructBinding> {
    model
        .modules
        .iter()
        .flat_map(|m| m.structs.iter())
        .map(|s| (s.name.clone(), s.clone()))
        .collect()
}

/// Materialize an *owned* C struct pointer (`ptr_expr`) into a plain JS object
/// assigned to `obj_var`, by invoking each generated field getter. The pointer
/// is consumed: after the fields are read it is destroyed, because the C ABI
/// hands back owned struct handles (the same ownership the other backends free).
#[allow(clippy::too_many_arguments)]
fn emit_struct_to_object(
    out: &mut String,
    env: &str,
    struct_name: &str,
    ptr_expr: &str,
    obj_var: &str,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
    indent: &str,
    destroy: bool,
) {
    let Some(def) = structs.get(local_type_name(struct_name)).cloned() else {
        // Unknown struct: fall back to the raw handle rather than emit broken C.
        out.push_str(&format!(
            "{indent}napi_create_int64({env}, (int64_t)(intptr_t){ptr_expr}, &{obj_var});\n"
        ));
        return;
    };
    let abi = &def.c_tag;
    let p = format!("{obj_var}_p");
    out.push_str(&format!("{indent}{{\n"));
    out.push_str(&format!("{indent}  {abi}* {p} = ({abi}*){ptr_expr};\n"));
    out.push_str(&format!(
        "{indent}  napi_create_object({env}, &{obj_var});\n"
    ));
    for field in &def.fields {
        let getter = &field.getter_symbol;
        let fv = format!("{obj_var}_{}", field.name);
        out.push_str(&format!("{indent}  napi_value {fv};\n"));
        emit_struct_field_to_napi(
            out,
            env,
            &field.ty,
            getter,
            &p,
            &fv,
            module,
            prefix,
            structs,
            &format!("{indent}  "),
        );
        out.push_str(&format!(
            "{indent}  napi_set_named_property({env}, {obj_var}, \"{}\", {fv});\n",
            field.name
        ));
    }
    if destroy {
        out.push_str(&format!("{indent}  {}({p});\n", def.destroy_symbol));
    }
    out.push_str(&format!("{indent}}}\n"));
}

/// The C statement that creates a napi value `target` from a leaf C expression
/// `expr` (scalars, bools, enums, handles). Strings/structs are handled by
/// [`emit_elem_to_napi`], which needs surrounding context.
fn napi_create_leaf(env: &str, ty: &TypeRef, expr: &str, target: &str) -> String {
    match ty {
        TypeRef::I32 => format!("napi_create_int32({env}, {expr}, &{target});"),
        TypeRef::U32 => format!("napi_create_uint32({env}, {expr}, &{target});"),
        TypeRef::I64 => format!("napi_create_int64({env}, {expr}, &{target});"),
        TypeRef::F64 => format!("napi_create_double({env}, {expr}, &{target});"),
        TypeRef::I8 | TypeRef::I16 => format!("napi_create_int32({env}, {expr}, &{target});"),
        TypeRef::U8 | TypeRef::U16 => format!("napi_create_uint32({env}, {expr}, &{target});"),
        TypeRef::U64 => format!("napi_create_int64({env}, (int64_t)({expr}), &{target});"),
        TypeRef::F32 => format!("napi_create_double({env}, {expr}, &{target});"),
        TypeRef::Bool => format!("napi_get_boolean({env}, {expr}, &{target});"),
        TypeRef::Enum(_) => format!("napi_create_int32({env}, (int32_t)({expr}), &{target});"),
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            format!("napi_create_int64({env}, (int64_t)(intptr_t)({expr}), &{target});")
        }
        _ => format!("napi_get_null({env}, &{target});"),
    }
}

/// Convert a single collection *element* C expression `expr` (a list item or map
/// value) into the napi value `target`. Owned elements are released after the
/// copy per their element plan: strings via `weaveffi_free_string`, record
/// elements via their `_destroy` (after materializing the JS object), and
/// rich-enum pointers surface as raw handles the JS wrapper class adopts.
#[allow(clippy::too_many_arguments)]
fn emit_elem_to_napi(
    out: &mut String,
    env: &str,
    ty: &TypeRef,
    expr: &str,
    target: &str,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
    indent: &str,
) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}napi_create_string_utf8({env}, {expr}, NAPI_AUTO_LENGTH, &{target});\n"
            ));
            if matches!(ty, TypeRef::StringUtf8) {
                out.push_str(&format!("{indent}weaveffi_free_string((char*)({expr}));\n"));
            }
        }
        TypeRef::Record(name) => {
            emit_struct_to_object(
                out, env, name, expr, target, module, prefix, structs, indent, true,
            );
        }
        TypeRef::RichEnum(_) => {
            out.push_str(&format!(
                "{indent}napi_create_int64({env}, (int64_t)(intptr_t)({expr}), &{target});\n"
            ));
        }
        _ => out.push_str(&format!(
            "{indent}{}\n",
            napi_create_leaf(env, ty, expr, target)
        )),
    }
}

/// Marshal one struct field, read via `getter(pv)`, into the JS value `fv`.
/// Scalars, enums, handles, owned strings, optional strings, nested structs,
/// byte buffers, lists, maps, and optional scalars are all materialized.
#[allow(clippy::too_many_arguments)]
fn emit_struct_field_to_napi(
    out: &mut String,
    env: &str,
    ty: &TypeRef,
    getter: &str,
    pv: &str,
    fv: &str,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
    indent: &str,
) {
    match ty {
        TypeRef::I32 => out.push_str(&format!(
            "{indent}napi_create_int32({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::U32 => out.push_str(&format!(
            "{indent}napi_create_uint32({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::I64 => out.push_str(&format!(
            "{indent}napi_create_int64({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::F64 => out.push_str(&format!(
            "{indent}napi_create_double({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::I8 | TypeRef::I16 => out.push_str(&format!(
            "{indent}napi_create_int32({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::U8 | TypeRef::U16 => out.push_str(&format!(
            "{indent}napi_create_uint32({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::U64 => out.push_str(&format!(
            "{indent}napi_create_int64({env}, (int64_t){getter}({pv}), &{fv});\n"
        )),
        TypeRef::F32 => out.push_str(&format!(
            "{indent}napi_create_double({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::Bool => out.push_str(&format!(
            "{indent}napi_get_boolean({env}, {getter}({pv}), &{fv});\n"
        )),
        TypeRef::Enum(_) => out.push_str(&format!(
            "{indent}napi_create_int32({env}, (int32_t){getter}({pv}), &{fv});\n"
        )),
        TypeRef::Handle | TypeRef::TypedHandle(_) => out.push_str(&format!(
            "{indent}napi_create_int64({env}, (int64_t)(intptr_t){getter}({pv}), &{fv});\n"
        )),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let owned = matches!(ty, TypeRef::StringUtf8);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!(
                "{indent}  char* {fv}_s = (char*){getter}({pv});\n"
            ));
            out.push_str(&format!(
                "{indent}  napi_create_string_utf8({env}, {fv}_s, NAPI_AUTO_LENGTH, &{fv});\n"
            ));
            if owned {
                out.push_str(&format!("{indent}  weaveffi_free_string({fv}_s);\n"));
            }
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Record(name) => {
            emit_struct_to_object(
                out,
                env,
                name,
                &format!("{getter}({pv})"),
                fv,
                module,
                prefix,
                structs,
                indent,
                true,
            );
        }
        // A rich-enum field getter hands back an owned pointer surfaced as a
        // raw handle; the JS side wraps it in the enum class, which owns
        // disposal.
        TypeRef::RichEnum(_) => {
            out.push_str(&format!(
                "{indent}napi_create_int64({env}, (int64_t)(intptr_t){getter}({pv}), &{fv});\n"
            ));
        }
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            let owned = matches!(inner.as_ref(), TypeRef::StringUtf8);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!(
                "{indent}  char* {fv}_s = (char*){getter}({pv});\n"
            ));
            out.push_str(&format!(
                "{indent}  if ({fv}_s == NULL) {{ napi_get_null({env}, &{fv}); }}\n"
            ));
            out.push_str(&format!(
                "{indent}  else {{ napi_create_string_utf8({env}, {fv}_s, NAPI_AUTO_LENGTH, &{fv});"
            ));
            if owned {
                out.push_str(&format!(" weaveffi_free_string({fv}_s);"));
            }
            out.push_str(" }\n");
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Record(_)) => {
            let TypeRef::Record(name) = inner.as_ref() else {
                unreachable!()
            };
            let abi = c_abi_struct_name(name, module, prefix);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!("{indent}  {abi}* {fv}_sp = {getter}({pv});\n"));
            out.push_str(&format!(
                "{indent}  if ({fv}_sp == NULL) {{ napi_get_null({env}, &{fv}); }}\n"
            ));
            out.push_str(&format!("{indent}  else {{\n"));
            emit_struct_to_object(
                out,
                env,
                name,
                &format!("{fv}_sp"),
                fv,
                module,
                prefix,
                structs,
                &format!("{indent}    "),
                true,
            );
            out.push_str(&format!("{indent}  }}\n"));
            out.push_str(&format!("{indent}}}\n"));
        }
        // An optional typed handle or rich enum lowers to a nullable opaque
        // pointer that the field surfaces as the integer handle (or null),
        // like the non-optional case but guarded on NULL.
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::TypedHandle(_) | TypeRef::RichEnum(_)
            ) =>
        {
            let (TypeRef::TypedHandle(name) | TypeRef::RichEnum(name)) = inner.as_ref() else {
                unreachable!()
            };
            let abi = c_abi_struct_name(name, module, prefix);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!("{indent}  {abi}* {fv}_h = {getter}({pv});\n"));
            out.push_str(&format!(
                "{indent}  if ({fv}_h == NULL) {{ napi_get_null({env}, &{fv}); }}\n"
            ));
            out.push_str(&format!(
                "{indent}  else {{ napi_create_int64({env}, (int64_t)(intptr_t){fv}_h, &{fv}); }}\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
        }
        // Remaining optionals (scalar/bool/enum/handle) lower to a nullable
        // pointer-to-value the getter returns directly.
        TypeRef::Optional(inner) => {
            let ct = c_elem_type(inner, module, prefix);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!("{indent}  {ct}* {fv}_p = {getter}({pv});\n"));
            out.push_str(&format!(
                "{indent}  if ({fv}_p == NULL) {{ napi_get_null({env}, &{fv}); }}\n"
            ));
            out.push_str(&format!(
                "{indent}  else {{ {} }}\n",
                napi_create_leaf(env, inner, &format!("*{fv}_p"), fv)
            ));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!("{indent}  size_t {fv}_len;\n"));
            out.push_str(&format!(
                "{indent}  const uint8_t* {fv}_data = (const uint8_t*){getter}({pv}, &{fv}_len);\n"
            ));
            out.push_str(&format!(
                "{indent}  if ({fv}_data == NULL) {{ napi_get_null({env}, &{fv}); }}\n"
            ));
            out.push_str(&format!(
                "{indent}  else {{ void* {fv}_buf; napi_create_buffer_copy({env}, {fv}_len, {fv}_data, &{fv}_buf, &{fv}); weaveffi_free_bytes((uint8_t*){fv}_data, {fv}_len); }}\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::List(inner) => {
            let et = c_elem_type(inner, module, prefix);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!("{indent}  size_t {fv}_len;\n"));
            out.push_str(&format!(
                "{indent}  {et}* {fv}_arr = {getter}({pv}, &{fv}_len);\n"
            ));
            out.push_str(&format!("{indent}  napi_create_array({env}, &{fv});\n"));
            out.push_str(&format!("{indent}  if ({fv}_arr != NULL) {{\n"));
            out.push_str(&format!(
                "{indent}    for (size_t {fv}_i = 0; {fv}_i < {fv}_len; {fv}_i++) {{\n"
            ));
            out.push_str(&format!("{indent}      napi_value {fv}_e;\n"));
            emit_elem_to_napi(
                out,
                env,
                inner,
                &format!("{fv}_arr[{fv}_i]"),
                &format!("{fv}_e"),
                module,
                prefix,
                structs,
                &format!("{indent}      "),
            );
            out.push_str(&format!(
                "{indent}      napi_set_element({env}, {fv}, (uint32_t){fv}_i, {fv}_e);\n"
            ));
            out.push_str(&format!("{indent}    }}\n"));
            out.push_str(&format!(
                "{indent}    weaveffi_free_bytes((uint8_t*){fv}_arr, {fv}_len * sizeof(*{fv}_arr));\n"
            ));
            out.push_str(&format!("{indent}  }}\n"));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Map(k, v) => {
            let kt = c_elem_type(k, module, prefix);
            let vt = c_elem_type(v, module, prefix);
            out.push_str(&format!("{indent}{{\n"));
            out.push_str(&format!("{indent}  {kt}* {fv}_keys = NULL;\n"));
            out.push_str(&format!("{indent}  {vt}* {fv}_vals = NULL;\n"));
            out.push_str(&format!("{indent}  size_t {fv}_len;\n"));
            out.push_str(&format!(
                "{indent}  {getter}({pv}, &{fv}_keys, &{fv}_vals, &{fv}_len);\n"
            ));
            emit_map_pairs_to_napi(
                out,
                env,
                k,
                v,
                &format!("{fv}_keys"),
                &format!("{fv}_vals"),
                &format!("{fv}_len"),
                fv,
                module,
                prefix,
                structs,
                &format!("{indent}  "),
            );
            out.push_str(&format!("{indent}}}\n"));
        }
        _ => out.push_str(&format!("{indent}napi_get_null({env}, &{fv});\n")),
    }
}

/// Build the JS object `target` from the parallel `keys`/`vals` arrays of a
/// returned map, releasing what the consumer owes: each copied string key or
/// value via `weaveffi_free_string`, then both producer-allocated arrays via
/// `weaveffi_free_bytes`.
#[allow(clippy::too_many_arguments)]
fn emit_map_pairs_to_napi(
    out: &mut String,
    env: &str,
    k: &TypeRef,
    v: &TypeRef,
    keys: &str,
    vals: &str,
    len: &str,
    target: &str,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
    indent: &str,
) {
    out.push_str(&format!("{indent}napi_create_object({env}, &{target});\n"));
    out.push_str(&format!(
        "{indent}if ({keys} != NULL && {vals} != NULL) {{\n"
    ));
    out.push_str(&format!(
        "{indent}  for (size_t {target}_i = 0; {target}_i < {len}; {target}_i++) {{\n"
    ));
    out.push_str(&format!("{indent}    napi_value {target}_v;\n"));
    emit_elem_to_napi(
        out,
        env,
        v,
        &format!("{vals}[{target}_i]"),
        &format!("{target}_v"),
        module,
        prefix,
        structs,
        &format!("{indent}    "),
    );
    match k {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}    napi_set_named_property({env}, {target}, {keys}[{target}_i], {target}_v);\n"
            ));
            if matches!(k, TypeRef::StringUtf8) {
                out.push_str(&format!(
                    "{indent}    weaveffi_free_string((char*){keys}[{target}_i]);\n"
                ));
            }
        }
        other => {
            out.push_str(&format!("{indent}    napi_value {target}_k;\n"));
            out.push_str(&format!(
                "{indent}    {}\n",
                napi_create_leaf(
                    env,
                    other,
                    &format!("{keys}[{target}_i]"),
                    &format!("{target}_k")
                )
            ));
            out.push_str(&format!(
                "{indent}    napi_set_property({env}, {target}, {target}_k, {target}_v);\n"
            ));
        }
    }
    out.push_str(&format!("{indent}  }}\n"));
    out.push_str(&format!("{indent}}}\n"));
    out.push_str(&format!(
        "{indent}weaveffi_free_bytes((uint8_t*){keys}, {len} * sizeof(*{keys}));\n"
    ));
    out.push_str(&format!(
        "{indent}weaveffi_free_bytes((uint8_t*){vals}, {len} * sizeof(*{vals}));\n"
    ));
}

fn emit_ret_to_napi(
    out: &mut String,
    ty: &TypeRef,
    module: &str,
    prefix: &str,
    f: &FnBinding,
    structs: &HashMap<String, StructBinding>,
) {
    out.push_str("  napi_value ret;\n");
    match ty {
        TypeRef::I32 => out.push_str("  napi_create_int32(env, result, &ret);\n"),
        TypeRef::U32 => out.push_str("  napi_create_uint32(env, result, &ret);\n"),
        TypeRef::I64 => out.push_str("  napi_create_int64(env, result, &ret);\n"),
        TypeRef::F64 => out.push_str("  napi_create_double(env, result, &ret);\n"),
        TypeRef::I8 | TypeRef::I16 => out.push_str("  napi_create_int32(env, result, &ret);\n"),
        TypeRef::U8 | TypeRef::U16 => out.push_str("  napi_create_uint32(env, result, &ret);\n"),
        TypeRef::U64 => out.push_str("  napi_create_int64(env, (int64_t)result, &ret);\n"),
        TypeRef::F32 => out.push_str("  napi_create_double(env, result, &ret);\n"),
        TypeRef::Bool => out.push_str("  napi_get_boolean(env, result, &ret);\n"),
        TypeRef::StringUtf8 => {
            out.push_str("  napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str("  weaveffi_free_string(result);\n");
        }
        TypeRef::BorrowedStr => {
            out.push_str("  napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
        }
        // A returned interface or rich enum is an owned object reference
        // surfaced as the raw handle; the JS loader wraps it in its class
        // (which owns disposal), so the addon must not destroy it here.
        TypeRef::TypedHandle(_) | TypeRef::Handle | TypeRef::Interface(_) | TypeRef::RichEnum(_) => {
            out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
        }
        TypeRef::Record(name) => {
            emit_struct_to_object(
                out, "env", name, "result", "ret", module, prefix, structs, "  ", true,
            );
        }
        TypeRef::Enum(_) => {
            out.push_str("  napi_create_int32(env, (int32_t)result, &ret);\n");
        }
        TypeRef::Bytes => {
            out.push_str("  napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
            out.push_str("  weaveffi_free_bytes((uint8_t*)result, out_len);\n");
        }
        TypeRef::BorrowedBytes => {
            out.push_str("  napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
        }
        TypeRef::Optional(inner) => {
            out.push_str("  if (result == NULL) {\n");
            out.push_str("    napi_get_null(env, &ret);\n");
            out.push_str("  } else {\n");
            emit_optional_ret_inner(out, inner, module, prefix, structs);
            out.push_str("  }\n");
        }
        TypeRef::List(inner) => emit_list_ret(out, inner, module, prefix, "  ", structs),
        TypeRef::Map(k, v) => {
            emit_map_pairs_to_napi(
                out,
                "env",
                k,
                v,
                "out_keys",
                "out_values",
                "out_len",
                "ret",
                module,
                prefix,
                structs,
                "  ",
            );
        }
        TypeRef::Iterator(_) => {
            // Lazy: the launcher's owned iterator handle is boxed into a
            // heap-allocated state cell and wrapped in a JS external. The
            // JS wrapper drives the per-iterator `next`/`destroy` entry
            // points one element at a time; the external's finalizer is the
            // safety net for abandoned iterators.
            let c_name = &f.c_base;
            out.push_str(&format!(
                "  {prefix}_napi_iter_state* iter_state = ({prefix}_napi_iter_state*)calloc(1, sizeof({prefix}_napi_iter_state));\n"
            ));
            out.push_str("  iter_state->iter = (void*)result;\n");
            out.push_str(&format!(
                "  napi_create_external(env, iter_state, {c_name}_napi_iter_finalize, NULL, &ret);\n"
            ));
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str("  return ret;\n");
}

/// Emit the present-case conversion of an optional return (the NULL case was
/// already handled by the caller). Pointer-lowered optionals (`is_c_pointer_type`)
/// reuse the inner type's return plan on `result` directly; everything else is
/// a producer-boxed scalar that is dereferenced and then released per
/// `ReturnFree::BoxedScalar` (`weaveffi_free_bytes(ptr, sizeof(T))`).
fn emit_optional_ret_inner(
    out: &mut String,
    inner: &TypeRef,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
) {
    // The boxed-scalar release owed after dereferencing the producer's box.
    let free_box = "    weaveffi_free_bytes((uint8_t*)result, sizeof(*result));\n";
    match inner {
        TypeRef::I32 => {
            out.push_str("    napi_create_int32(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::U32 => {
            out.push_str("    napi_create_uint32(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::I64 => {
            out.push_str("    napi_create_int64(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::F64 => {
            out.push_str("    napi_create_double(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::I8 | TypeRef::I16 => {
            out.push_str("    napi_create_int32(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::U8 | TypeRef::U16 => {
            out.push_str("    napi_create_uint32(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::U64 => {
            out.push_str("    napi_create_int64(env, (int64_t)*result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::F32 => {
            out.push_str("    napi_create_double(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::Bool => {
            out.push_str("    napi_get_boolean(env, *result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::Handle => {
            out.push_str("    napi_create_int64(env, (int64_t)*result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::Enum(_) => {
            out.push_str("    napi_create_int32(env, (int32_t)*result, &ret);\n");
            out.push_str(free_box);
        }
        TypeRef::StringUtf8 => {
            out.push_str("    napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str("    weaveffi_free_string(result);\n");
        }
        TypeRef::BorrowedStr => {
            out.push_str("    napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
        }
        TypeRef::Bytes => {
            out.push_str("    napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
            out.push_str("    weaveffi_free_bytes((uint8_t*)result, out_len);\n");
        }
        TypeRef::BorrowedBytes => {
            out.push_str("    napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
        }
        TypeRef::Record(name) => {
            emit_struct_to_object(
                out, "env", name, "result", "ret", module, prefix, structs, "    ", true,
            );
        }
        // An optional interface, rich enum, or typed handle lowers to one
        // nullable pointer, so `result` is the object itself: surface the raw
        // handle without dereferencing or freeing it (the JS wrapper class
        // adopts owned references and owns their disposal).
        TypeRef::Interface(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => {
            out.push_str("    napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
        }
        TypeRef::List(li) => emit_list_ret(out, li, module, prefix, "    ", structs),
        _ => out.push_str("    napi_get_null(env, &ret);\n"),
    }
}

fn emit_list_ret(
    out: &mut String,
    inner: &TypeRef,
    module: &str,
    prefix: &str,
    ind: &str,
    structs: &HashMap<String, StructBinding>,
) {
    out.push_str(&format!(
        "{ind}napi_create_array_with_length(env, out_len, &ret);\n"
    ));
    out.push_str(&format!(
        "{ind}for (size_t ret_i = 0; ret_i < out_len; ret_i++) {{\n"
    ));
    out.push_str(&format!("{ind}  napi_value elem;\n"));
    match inner {
        TypeRef::I32 => out.push_str(&format!(
            "{ind}  napi_create_int32(env, result[ret_i], &elem);\n"
        )),
        TypeRef::U32 => out.push_str(&format!(
            "{ind}  napi_create_uint32(env, result[ret_i], &elem);\n"
        )),
        TypeRef::I64 => out.push_str(&format!(
            "{ind}  napi_create_int64(env, result[ret_i], &elem);\n"
        )),
        TypeRef::F64 => out.push_str(&format!(
            "{ind}  napi_create_double(env, result[ret_i], &elem);\n"
        )),
        TypeRef::I8 | TypeRef::I16 => out.push_str(&format!(
            "{ind}  napi_create_int32(env, result[ret_i], &elem);\n"
        )),
        TypeRef::U8 | TypeRef::U16 => out.push_str(&format!(
            "{ind}  napi_create_uint32(env, result[ret_i], &elem);\n"
        )),
        TypeRef::U64 => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)result[ret_i], &elem);\n"
        )),
        TypeRef::F32 => out.push_str(&format!(
            "{ind}  napi_create_double(env, result[ret_i], &elem);\n"
        )),
        TypeRef::Bool => out.push_str(&format!(
            "{ind}  napi_get_boolean(env, result[ret_i], &elem);\n"
        )),
        TypeRef::TypedHandle(_) | TypeRef::Handle => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)(intptr_t)result[ret_i], &elem);\n"
        )),
        TypeRef::StringUtf8 => {
            out.push_str(&format!(
                "{ind}  napi_create_string_utf8(env, result[ret_i], NAPI_AUTO_LENGTH, &elem);\n"
            ));
            out.push_str(&format!("{ind}  weaveffi_free_string(result[ret_i]);\n"));
        }
        TypeRef::Enum(_) => out.push_str(&format!(
            "{ind}  napi_create_int32(env, (int32_t)result[ret_i], &elem);\n"
        )),
        TypeRef::Record(name) => {
            let elem_indent = format!("{ind}  ");
            emit_struct_to_object(
                out,
                "env",
                name,
                "result[ret_i]",
                "elem",
                module,
                prefix,
                structs,
                &elem_indent,
                true,
            );
        }
        // Rich-enum elements cross as owned raw handles; the consumer wraps
        // them in the enum class, which owns disposal.
        TypeRef::RichEnum(_) => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)(intptr_t)result[ret_i], &elem);\n"
        )),
        _ => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)result[ret_i], &elem);\n"
        )),
    }
    out.push_str(&format!(
        "{ind}  napi_set_element(env, ret, (uint32_t)ret_i, elem);\n"
    ));
    out.push_str(&format!("{ind}}}\n"));
    // The array buffer itself is producer-allocated; release it with the
    // runtime's own deallocator, per `ReturnFree::Array`.
    out.push_str(&format!(
        "{ind}weaveffi_free_bytes((uint8_t*)result, out_len * sizeof(*result));\n"
    ));
}

fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Buffer".into(),
        TypeRef::Handle => "bigint".into(),
        // Records, rich enums, plain enums, interfaces, and typed handles
        // surface as bare local TS names. A cross-module reference (e.g.
        // `handle<Store>` resolved to `kv.Store`) must annotate the *local*
        // type `Store`; the qualified IR name is not a declared TS type in
        // this module.
        TypeRef::TypedHandle(name) => local_type_name(name).to_string(),
        TypeRef::Record(name) | TypeRef::RichEnum(name) => local_type_name(name).to_string(),
        TypeRef::Interface(name) => local_type_name(name).to_string(),
        TypeRef::Enum(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        TypeRef::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), TypeRef::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        TypeRef::Map(k, v) => format!("Record<{}, {}>", ts_type_for(k), ts_type_for(v)),
        // `iter<T>` is a lazy pull stream, not a materialized array.
        TypeRef::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("IterableIterator<{t}>")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
    params: &[ParamBinding],
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
                out.push_str(&format!(" * @param {} {}\n", js_param_name(&p.name), first));
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

/// `.d.ts` for a rich (algebraic) enum: a class with a static factory per
/// variant (`Shape.circle(radius)`), a `tag()` discriminant reader, a frozen
/// `Tag` discriminant map, per-variant namespaced field getters
/// (`circleRadius`), and `destroy()`. Mirrors the JS class in [`render_node_index`].
fn render_rich_enum_dts(out: &mut String, e: &EnumBinding) {
    let Some(rich) = &e.rich else {
        return;
    };
    let name = &e.name;
    let mut w = CodeWriter::two_space();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("export class {name} {{"), "}", |w| {
        for v in &rich.variants {
            let factory = v.name.to_lower_camel_case();
            let params: Vec<String> = v
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, ts_type_for(&f.ty)))
                .collect();
            let mut d = String::new();
            emit_doc(&mut d, &v.doc, "  ");
            w.raw(d);
            w.line(format!("static {factory}({}): {name};", params.join(", ")));
        }
        w.line("/** The active variant's discriminant. */");
        w.line("tag(): number;");
        for v in &rich.variants {
            for f in &v.fields {
                let getter = format!(
                    "{}{}",
                    v.name.to_lower_camel_case(),
                    f.name.to_upper_camel_case()
                );
                let mut d = String::new();
                emit_doc(&mut d, &f.doc, "  ");
                w.raw(d);
                w.line(format!("get {getter}(): {};", ts_type_for(&f.ty)));
            }
        }
        w.line("/** Free the underlying native object. */");
        w.line("destroy(): void;");
    });
    // The discriminant map, e.g. `Shape.Tag.Circle === 1`.
    w.block(format!("export namespace {name} {{"), "}", |w| {
        w.block("const Tag: Readonly<{", "}>;", |w| {
            for v in &e.variants {
                w.line(format!("{}: {},", v.name, v.value));
            }
        });
    });
    out.push_str(&w.finish());
}

fn render_struct_builder_dts(out: &mut String, s: &StructBinding) {
    let name = &s.name;
    let mut w = CodeWriter::two_space();
    {
        let mut d = String::new();
        emit_doc(&mut d, &s.doc, "");
        w.raw(d);
    }
    w.block(format!("export interface {}Builder {{", s.name), "}", |w| {
        for field in &s.fields {
            let method = format!("with{}", field.name.to_upper_camel_case());
            let ts = ts_type_for(&field.ty);
            let mut d = String::new();
            emit_doc(&mut d, &field.doc, "  ");
            w.raw(d);
            w.line(format!("{method}(value: {ts}): {name}Builder;"));
        }
        w.line(format!("build(): {name};"));
    });
    out.push_str(&w.finish());
}

/// How a wrapper rebuilds a class-typed result from the raw addon value.
struct RetWrap {
    /// The local JS class name.
    cls: String,
    /// `true` for `T?`: the addon surfaces `null` for the absent case.
    optional: bool,
    /// `true` for an interface (wrap via `_fromHandle`); `false` for a rich
    /// enum (wrap via the class constructor).
    iface: bool,
}

/// Recognize a class-typed return carried directly or as an `Optional`.
/// Deeper nestings (list/map elements) are rejected by validation for
/// interfaces and flow through unwrapped for rich enums.
fn js_ret_wrap(ret: Option<&TypeRef>) -> Option<RetWrap> {
    fn direct(ty: &TypeRef, optional: bool) -> Option<RetWrap> {
        match ty {
            TypeRef::RichEnum(n) => Some(RetWrap {
                cls: local_type_name(n).to_string(),
                optional,
                iface: false,
            }),
            TypeRef::Interface(n) => Some(RetWrap {
                cls: local_type_name(n).to_string(),
                optional,
                iface: true,
            }),
            _ => None,
        }
    }
    match ret? {
        TypeRef::Optional(inner) => direct(inner, true),
        ty => direct(ty, false),
    }
}

/// The addon-argument expression for one logical parameter: interface and
/// rich-enum instances are unwrapped to their raw `_handle` (a borrow; the
/// callee never takes ownership), everything else passes through.
fn js_arg_expr(js_name: &str, ty: &TypeRef) -> String {
    fn wrapper_class(ty: &TypeRef) -> Option<&str> {
        match ty {
            TypeRef::RichEnum(n) | TypeRef::Interface(n) => Some(local_type_name(n)),
            _ => None,
        }
    }
    let cls = match ty {
        TypeRef::Optional(inner) => wrapper_class(inner),
        ty => wrapper_class(ty),
    };
    match cls {
        Some(c) => format!("{js_name} instanceof {c} ? {js_name}._handle : {js_name}"),
        None => js_name.to_string(),
    }
}

/// The rebranding factory a callable's failures route through: the declaring
/// module's domain factory when the callable `throws`, the generic
/// [`ERROR_BRAND`] constructor otherwise (panics and marshalling failures).
fn js_error_map_expr(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => js_error_factory_name(eb),
        _ => "__generic".to_string(),
    }
}

/// `__kvErrorFrom`, the code-to-class factory of the domain declared by
/// `owner_path`. Derived from the owner so inheriting submodules name the
/// same function.
fn js_error_factory_name(eb: &ErrorBinding) -> String {
    format!("__{}ErrorFrom", eb.owner_path.to_lower_camel_case())
}

/// Emit one declaring module's typed error surface onto `wv`: the domain
/// class extending the generic brand, one subclass per code carrying its
/// stable `CODE` and default message, and the factory mapping a raw ABI code
/// to the matching class (or the generic brand for codes outside the domain:
/// panics and marshalling failures).
fn render_error_classes_js(out: &mut String, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = js_error_factory_name(eb);
    let table = format!("__{}ErrorCodes", eb.owner_path.to_lower_camel_case());

    let mut w = CodeWriter::two_space();
    w.block(
        format!("class {domain} extends {ERROR_BRAND} {{"),
        "}",
        |w| {
            w.block("constructor(code, message) {", "}", |w| {
                w.line("super(code, message);");
                w.line(format!("this.name = '{domain}';"));
            });
        },
    );
    w.line(format!("wv.{domain} = {domain};"));
    for c in &eb.codes {
        let class = error_type_name(&c.name, "Error");
        let default_msg = js_str_literal(&c.message);
        w.block(format!("class {class} extends {domain} {{"), "}", |w| {
            w.block("constructor(message) {", "}", |w| {
                w.line(format!("super({}, message || '{default_msg}');", c.value));
                w.line(format!("this.name = '{class}';"));
            });
        });
        w.line(format!("{class}.CODE = {};", c.value));
        w.line(format!("wv.{class} = {class};"));
    }
    let entries: Vec<String> = eb
        .codes
        .iter()
        .map(|c| format!("{}: {}", c.value, error_type_name(&c.name, "Error")))
        .collect();
    w.line(format!(
        "const {table} = Object.freeze({{ {} }});",
        entries.join(", ")
    ));
    w.block(format!("function {factory}(code, message) {{"), "}", |w| {
        w.line(format!("const _cls = {table}[code];"));
        w.line(format!(
            "return _cls === undefined ? new {ERROR_BRAND}(code, message) : new _cls(message);"
        ));
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit one wrapper callable's body: unwrap class-typed arguments to raw
/// handles, invoke the addon binding through the rebranding helper, and wrap
/// a class-typed result. Iterator-returning callables launch the native
/// iterator and hand its external to the shared lazy iterator class. Shared
/// by free functions and interface members (`self_expr` supplies the leading
/// handle of an instance method).
fn emit_wrapper_body_js(
    w: &mut CodeWriter,
    f: &FnBinding,
    addon_name: &str,
    self_expr: Option<&str>,
    map_expr: &str,
) {
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = self_expr {
        args.push(s.to_string());
    }
    for p in &f.params {
        args.push(js_arg_expr(&js_param_name(&p.name), &p.ty));
    }
    let args = args.join(", ");
    let invoke = if f.is_async {
        "__invokeAsync"
    } else {
        "__invoke"
    };
    let call = format!("{invoke}(addon.{addon_name}, [{args}], {map_expr})");

    if let Some(TypeRef::Iterator(inner)) = f.ret.as_ref() {
        // Launch, then wrap the external in the lazy iterator: one native
        // `next` per consumer step, `destroy` on exhaustion or early exit.
        // Rich-enum elements arrive as owned raw handles the class adopts.
        let wrap_elem = match inner.as_ref() {
            TypeRef::RichEnum(n) => format!("(_e) => new {}(_e)", local_type_name(n)),
            _ => "null".to_string(),
        };
        w.line(format!("const _it = {call};"));
        w.line(format!(
            "return new WeaveFFIIterator(_it, addon.{addon_name}_iterNext, addon.{addon_name}_iterDestroy, {map_expr}, {wrap_elem});"
        ));
        return;
    }

    let Some(wrap) = js_ret_wrap(f.ret.as_ref()) else {
        w.line(format!("return {call};"));
        return;
    };
    let cls = &wrap.cls;
    let rewrap = if wrap.iface {
        format!("{cls}._fromHandle(_r)")
    } else {
        format!("new {cls}(_r)")
    };
    match (f.is_async, wrap.optional) {
        (false, false) => {
            w.line(format!("const _r = {call};"));
            w.line(format!("return {rewrap};"));
        }
        (false, true) => {
            w.line(format!("const _r = {call};"));
            w.line(format!("return _r == null ? null : {rewrap};"));
        }
        (true, false) => {
            w.line(format!("return {call}.then((_r) => {rewrap});"));
        }
        (true, true) => {
            w.line(format!(
                "return {call}.then((_r) => (_r == null ? null : {rewrap}));"
            ));
        }
    }
}

/// Emit one interface's JS class onto `wv`, following the rich-enum wrapper
/// pattern: the class owns the opaque handle and frees it once, via explicit
/// `destroy()` or a `FinalizationRegistry` safety net. A sync constructor
/// named `new` becomes the JS `constructor`; every other constructor becomes
/// a static factory; methods pass the wrapped handle as the leading addon
/// argument; statics are static methods.
fn render_interface_class_js(out: &mut String, i: &InterfaceBinding, m: &ModuleBinding, strip: bool) {
    let name = &i.name;
    let destroy_js = wrapper_name(&m.path, &iface_member_base(name, "destroy"), strip);
    let error = m.error.as_ref();

    let mut w = CodeWriter::two_space();
    w.block(format!("class {name} {{"), "}", |w| {
        let canonical = i
            .constructors
            .iter()
            .find(|c| c.name == "new" && !c.is_async);
        if let Some(c) = canonical {
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &c.name), strip);
            let params: Vec<String> = c.params.iter().map(|p| js_param_name(&p.name)).collect();
            let args: Vec<String> = c
                .params
                .iter()
                .map(|p| js_arg_expr(&js_param_name(&p.name), &p.ty))
                .collect();
            let map = js_error_map_expr(c, error);
            w.block(format!("constructor({}) {{", params.join(", ")), "}", |w| {
                w.line(format!(
                    "this._handle = __invoke(addon.{addon_name}, [{}], {map});",
                    args.join(", ")
                ));
                w.line(format!(
                    "{name}._cleanup.register(this, this._handle, this);"
                ));
            });
        }
        for c in &i.constructors {
            if canonical.is_some_and(|canon| std::ptr::eq(canon, c)) {
                continue;
            }
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &c.name), strip);
            let factory = c.name.to_lower_camel_case();
            let params: Vec<String> = c.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(c, error);
            w.block(
                format!("static {factory}({}) {{", params.join(", ")),
                "}",
                |w| {
                    emit_wrapper_body_js(w, c, &addon_name, None, &map);
                },
            );
        }
        for f in &i.methods {
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &f.name), strip);
            let method = f.name.to_lower_camel_case();
            let params: Vec<String> = f.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(f, error);
            w.block(format!("{method}({}) {{", params.join(", ")), "}", |w| {
                emit_wrapper_body_js(w, f, &addon_name, Some("this._handle"), &map);
            });
        }
        for f in &i.statics {
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &f.name), strip);
            let method = f.name.to_lower_camel_case();
            let params: Vec<String> = f.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(f, error);
            w.block(
                format!("static {method}({}) {{", params.join(", ")),
                "}",
                |w| {
                    emit_wrapper_body_js(w, f, &addon_name, None, &map);
                },
            );
        }
        // Explicit cleanup; guarded so a double `destroy()` (or destroy-then-GC)
        // is a no-op rather than a double free.
        w.block("destroy() {", "}", |w| {
            w.block("if (this._handle) {", "}", |w| {
                w.line(format!("{name}._cleanup.unregister(this);"));
                w.line(format!("addon.{destroy_js}(this._handle);"));
                w.line("this._handle = 0;");
            });
        });
    });

    // Wrap an owned handle returned by the addon without running the public
    // constructor (which would invoke the native constructor again).
    w.block(
        format!("{name}._fromHandle = function (handle) {{"),
        "};",
        |w| {
            w.line(format!("const _o = Object.create({name}.prototype);"));
            w.line("_o._handle = handle;");
            w.line(format!("{name}._cleanup.register(_o, handle, _o);"));
            w.line("return _o;");
        },
    );
    w.block(
        format!("{name}._cleanup = new FinalizationRegistry((handle) => {{"),
        "});",
        |w| {
            w.line(format!("if (handle) {{ addon.{destroy_js}(handle); }}"));
        },
    );
    w.line(format!("wv.{name} = {name};"));
    w.blank();
    out.push_str(&w.finish());
}

/// True when any callable in the model returns `iter<T>`, so the addon and
/// loader must emit the shared lazy-iterator support.
fn model_has_iterators(model: &BindingModel) -> bool {
    model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| matches!(f.shape, CallShape::Iterator(_))))
}

/// Emit the shared lazy iterator class the JS loader hands out for every
/// `iter<T>` callable. It implements the iterator protocol over the addon's
/// per-iterator `next`/`destroy` entry points: one native pull per `next()`,
/// eager release on exhaustion (the addon destroys the handle when the
/// producer reports done), and `return()` releases the handle on early exit
/// so `for...of` breaks clean up deterministically. Abandoned iterators are
/// reclaimed by the external's native finalizer.
fn render_iterator_class_js(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.raw(
        "// Lazy iterator over a native producer: one native `next` per step.\n\
         // The native handle is released on exhaustion, by `return()` on early\n\
         // exit, or by the external's finalizer if the iterator is abandoned.\n",
    );
    w.block("class WeaveFFIIterator {", "}", |w| {
        w.block("constructor(ext, nextFn, destroyFn, map, wrapElem) {", "}", |w| {
            w.line("this._ext = ext;");
            w.line("this._nextFn = nextFn;");
            w.line("this._destroyFn = destroyFn;");
            w.line("this._map = map;");
            w.line("this._wrapElem = wrapElem;");
            w.line("this._done = false;");
        });
        w.block("next() {", "}", |w| {
            w.block("if (this._done) {", "}", |w| {
                w.line("return { done: true, value: undefined };");
            });
            w.line("const _v = __invoke(this._nextFn, [this._ext], this._map);");
            w.block("if (_v === undefined) {", "}", |w| {
                w.line("this._done = true;");
                w.line("return { done: true, value: undefined };");
            });
            w.line("return { done: false, value: this._wrapElem ? this._wrapElem(_v) : _v };");
        });
        w.block("return(value) {", "}", |w| {
            w.block("if (!this._done) {", "}", |w| {
                w.line("this._done = true;");
                w.line("this._destroyFn(this._ext);");
            });
            w.line("return { done: true, value };");
        });
        w.block("[Symbol.iterator]() {", "}", |w| {
            w.line("return this;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The JS loader (`index.js`). Re-exports the native addon's bindings, then
/// layers the idiomatic surface on top: the generic error brand plus one
/// typed error class per declared domain, wrapper classes for rich enums and
/// interfaces, and one wrapper per module function so failures rebrand as the
/// right error class and class-typed values cross as instances rather than
/// raw handles.
fn render_node_index(model: &BindingModel, strip: bool, input_basename: &str) -> String {
    let dbl = CommentStyle::DoubleSlash;
    let mut out = render_prelude(dbl, input_basename);
    out.push_str(
        "// The WEAVEFFI_ADDON environment variable overrides the addon location\n\
         // (an absolute path to the built .node file); otherwise prefer the\n\
         // default node-gyp output path and fall back to a prebuilt index.node\n\
         // placed next to this file.\n\
         let addon;\n\
         if (process.env.WEAVEFFI_ADDON) {\n  addon = require(process.env.WEAVEFFI_ADDON);\n} else {\n  try {\n    addon = require('./build/Release/weaveffi.node');\n  } catch (e) {\n    addon = require('./index.node');\n  }\n}\n",
    );

    // The native bindings are defined as non-enumerable properties, so copy
    // them by explicit own-name lookup before layering the idiomatic wrappers.
    out.push_str(
        "\n// Re-export every native binding, then layer the idiomatic wrappers\n\
         // (error classes, interface and rich-enum classes, function wrappers)\n\
         // on top.\n\
         const wv = {};\n\
         for (const _name of Object.getOwnPropertyNames(addon)) {\n  wv[_name] = addon[_name];\n}\n\n",
    );

    // The generic brand and the shared invoke helpers. Every wrapper funnels
    // addon failures (JS errors carrying the numeric ABI `code`) through a
    // mapping factory: the module domain's for throwing callables, the
    // generic constructor otherwise.
    out.push_str(&format!(
        "class {ERROR_BRAND} extends Error {{\n  \
           constructor(code, message) {{\n    \
             super('(' + code + ') ' + (message || ''));\n    \
             this.name = '{ERROR_BRAND}';\n    \
             this.code = code;\n    \
             this.errorMessage = message || '';\n  \
           }}\n\
         }}\n\
         wv.{ERROR_BRAND} = {ERROR_BRAND};\n\
         function __generic(code, message) {{\n  \
           return new {ERROR_BRAND}(code, message);\n\
         }}\n\
         function __rebrand(e, map) {{\n  \
           return e && typeof e.code === 'number' ? map(e.code, e.message) : e;\n\
         }}\n\
         function __invoke(fn, args, map) {{\n  \
           try {{\n    \
             return fn.apply(null, args);\n  \
           }} catch (e) {{\n    \
             throw __rebrand(e, map);\n  \
           }}\n\
         }}\n\
         function __invokeAsync(fn, args, map) {{\n  \
           return fn.apply(null, args).catch((e) => {{\n    \
             throw __rebrand(e, map);\n  \
           }});\n\
         }}\n\n"
    ));

    if model_has_iterators(model) {
        render_iterator_class_js(&mut out);
    }

    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error_classes_js(&mut out, eb);
        }
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum_class_js(&mut out, e, &m.path, strip);
            }
        }
        for i in &m.interfaces {
            render_interface_class_js(&mut out, i, m, strip);
        }
    }

    // One wrapper per module function, so every failure is rebranded and
    // class-typed parameters and returns cross as instances.
    for m in &model.modules {
        for f in &m.functions {
            let js = js_fn_name(&m.path, &f.name, strip);
            let params: Vec<String> = f.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(f, m.error.as_ref());
            let mut w = CodeWriter::two_space();
            w.block(
                format!("wv.{js} = function ({}) {{", params.join(", ")),
                "};",
                |w| {
                    emit_wrapper_body_js(w, f, &js, None, &map);
                },
            );
            out.push_str(&w.finish());
        }
    }

    out.push_str("\nmodule.exports = wv;\n\n");
    out.push_str(&render_trailer(dbl, "index.js"));
    out
}

/// Emit one rich-enum wrapper class onto `wv`. The class owns the opaque handle
/// and frees it once, via explicit `destroy()` or a `FinalizationRegistry`
/// safety net, mirroring how the other backends free the same object.
fn render_rich_enum_class_js(out: &mut String, e: &EnumBinding, module: &str, strip: bool) {
    let Some(rich) = &e.rich else {
        return;
    };
    let name = &e.name;
    let destroy_js = wrapper_name(module, &rich_destroy_base(name), strip);

    let mut w = CodeWriter::two_space();
    w.block(format!("class {name} {{"), "}", |w| {
        w.block("constructor(handle) {", "}", |w| {
            w.line("this._handle = handle;");
            w.line(format!("{name}._cleanup.register(this, handle, this);"));
        });

        // Per-variant factories (`Shape.circle(radius)`). A constructor
        // failure can only be marshalling or a panic, so it rebrands generic.
        for v in &rich.variants {
            let factory = v.name.to_lower_camel_case();
            let ctor_js = wrapper_name(module, &rich_ctor_base(name, &v.name), strip);
            let params: Vec<String> = v.fields.iter().map(|f| f.name.clone()).collect();
            let joined = params.join(", ");
            w.block(format!("static {factory}({joined}) {{"), "}", |w| {
                w.line(format!(
                    "return new {name}(__invoke(addon.{ctor_js}, [{joined}], __generic));"
                ));
            });
        }

        // Discriminant reader.
        let tag_js = wrapper_name(module, &rich_tag_base(name), strip);
        w.block("tag() {", "}", |w| {
            w.line(format!("return addon.{tag_js}(this._handle);"));
        });

        // Namespaced per-variant field getters (`circleRadius`).
        for v in &rich.variants {
            for f in &v.fields {
                let getter = format!(
                    "{}{}",
                    v.name.to_lower_camel_case(),
                    f.name.to_upper_camel_case()
                );
                let getter_js =
                    wrapper_name(module, &rich_getter_base(name, &v.name, &f.name), strip);
                w.block(format!("get {getter}() {{"), "}", |w| {
                    w.line(format!("return addon.{getter_js}(this._handle);"));
                });
            }
        }

        // Explicit cleanup; guarded so a double `destroy()` (or destroy-then-GC) is
        // a no-op rather than a double free.
        w.block("destroy() {", "}", |w| {
            w.block("if (this._handle) {", "}", |w| {
                w.line(format!("{name}._cleanup.unregister(this);"));
                w.line(format!("addon.{destroy_js}(this._handle);"));
                w.line("this._handle = 0;");
            });
        });
    });

    w.block(
        format!("{name}._cleanup = new FinalizationRegistry((handle) => {{"),
        "});",
        |w| {
            w.line(format!("if (handle) {{ addon.{destroy_js}(handle); }}"));
        },
    );

    // Frozen discriminant map (`Shape.Tag.Circle === 1`).
    let consts: Vec<String> = e
        .variants
        .iter()
        .map(|v| format!("{}: {}", v.name, v.value))
        .collect();
    w.line(format!(
        "{name}.Tag = Object.freeze({{ {} }});",
        consts.join(", ")
    ));
    w.line(format!("wv.{name} = {name};"));
    w.blank();
    out.push_str(&w.finish());
}

/// The TS parameter list of a callable, camel-cased.
fn ts_params(f: &FnBinding) -> String {
    f.params
        .iter()
        .map(|p| format!("{}: {}", js_param_name(&p.name), ts_type_for(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The TS return annotation of a callable (`Promise`-wrapped when async).
fn ts_ret(f: &FnBinding) -> String {
    let base = match &f.ret {
        Some(ty) => ts_type_for(ty),
        None => "void".into(),
    };
    if f.is_async {
        format!("Promise<{base}>")
    } else {
        base
    }
}

/// The standard JSDoc tag list of a callable: the C mapping, a `@throws` tag
/// naming the module's domain class for throwing callables, and any
/// deprecation notice.
fn ts_fn_tags(f: &FnBinding, error: Option<&ErrorBinding>) -> Vec<String> {
    let mut tags = vec![format!("Maps to C function: {}", f.c_base)];
    if let (true, Some(eb)) = (f.throws, error) {
        tags.push(format!("@throws {{{}}}", eb.type_name));
    }
    if let Some(msg) = &f.deprecated {
        tags.push(format!("@deprecated {}", msg));
    }
    tags
}

/// `.d.ts` for one declaring module's error surface: the domain class
/// extending the generic brand plus one subclass per code carrying its
/// stable `CODE`.
fn render_error_dts(out: &mut String, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    out.push_str(&format!(
        "/** Typed errors reported by the `{}` module's throwing functions. */\n",
        eb.owner_path
    ));
    out.push_str(&format!("export class {domain} extends {ERROR_BRAND} {{\n"));
    out.push_str("  constructor(code: number, message: string);\n");
    out.push_str("}\n");
    for c in &eb.codes {
        let class = error_type_name(&c.name, "Error");
        emit_doc(out, &c.doc, "");
        out.push_str(&format!("export class {class} extends {domain} {{\n"));
        out.push_str(&format!("  static readonly CODE: {};\n", c.value));
        out.push_str("  constructor(message?: string);\n");
        out.push_str("}\n");
    }
}

/// `.d.ts` for one interface: a class whose canonical `new` constructor,
/// static factories, methods, and statics mirror the JS class in
/// [`render_interface_class_js`].
fn render_interface_dts(out: &mut String, i: &InterfaceBinding, error: Option<&ErrorBinding>) {
    let name = &i.name;
    let mut w = CodeWriter::two_space();
    {
        let mut d = String::new();
        emit_doc(&mut d, &i.doc, "");
        w.raw(d);
    }
    w.block(format!("export class {name} {{"), "}", |w| {
        let canonical = i
            .constructors
            .iter()
            .find(|c| c.name == "new" && !c.is_async);
        if let Some(c) = canonical {
            let mut d = String::new();
            emit_fn_doc(&mut d, &c.doc, &c.params, "  ", &ts_fn_tags(c, error));
            w.raw(d);
            w.line(format!("constructor({});", ts_params(c)));
        }
        for c in &i.constructors {
            if canonical.is_some_and(|canon| std::ptr::eq(canon, c)) {
                continue;
            }
            let mut d = String::new();
            emit_fn_doc(&mut d, &c.doc, &c.params, "  ", &ts_fn_tags(c, error));
            w.raw(d);
            let ret = if c.is_async {
                format!("Promise<{name}>")
            } else {
                name.to_string()
            };
            w.line(format!(
                "static {}({}): {ret};",
                c.name.to_lower_camel_case(),
                ts_params(c)
            ));
        }
        for f in &i.methods {
            let mut d = String::new();
            emit_fn_doc(&mut d, &f.doc, &f.params, "  ", &ts_fn_tags(f, error));
            w.raw(d);
            w.line(format!(
                "{}({}): {};",
                f.name.to_lower_camel_case(),
                ts_params(f),
                ts_ret(f)
            ));
        }
        for f in &i.statics {
            let mut d = String::new();
            emit_fn_doc(&mut d, &f.doc, &f.params, "  ", &ts_fn_tags(f, error));
            w.raw(d);
            w.line(format!(
                "static {}({}): {};",
                f.name.to_lower_camel_case(),
                ts_params(f),
                ts_ret(f)
            ));
        }
        w.line("/** Free the underlying native object. */");
        w.line("destroy(): void;");
    });
    out.push_str(&w.finish());
}

fn render_node_dts(
    model: &BindingModel,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str("// Generated types for WeaveFFI functions\n");
    out.push_str("/**\n");
    out.push_str(" * Base class of every error thrown by these bindings. Non-throwing\n");
    out.push_str(" * functions reject or throw it directly for panics and marshalling\n");
    out.push_str(" * failures; throwing functions surface a module domain subclass.\n");
    out.push_str(" */\n");
    out.push_str(&format!("export class {ERROR_BRAND} extends Error {{\n"));
    out.push_str("  /** The numeric ABI error code. */\n");
    out.push_str("  code: number;\n");
    out.push_str("  /** The raw producer message, without the code prefix. */\n");
    out.push_str("  errorMessage: string;\n");
    out.push_str("  constructor(code: number, message: string);\n");
    out.push_str("}\n");
    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error_dts(&mut out, eb);
        }
        for s in &m.structs {
            emit_doc(&mut out, &s.doc, "");
            out.push_str(&format!("export interface {} {{\n", s.name));
            for field in &s.fields {
                emit_doc(&mut out, &field.doc, "  ");
                out.push_str(&format!("  {}: {};\n", field.name, ts_type_for(&field.ty)));
            }
            out.push_str("}\n");
            if s.builder.is_some() {
                render_struct_builder_dts(&mut out, s);
            }
        }
        for e in &m.enums {
            // A rich (algebraic) enum is an opaque-object wrapper class, not a
            // plain numeric `enum`.
            if e.is_rich() {
                render_rich_enum_dts(&mut out, e);
                continue;
            }
            emit_doc(&mut out, &e.doc, "");
            out.push_str(&format!("export enum {} {{\n", e.name));
            for v in &e.variants {
                emit_doc(&mut out, &v.doc, "  ");
                out.push_str(&format!("  {} = {},\n", v.name, v.value));
            }
            out.push_str("}\n");
        }
        out.push_str(&format!("// module {}\n", m.path));
        for i in &m.interfaces {
            render_interface_dts(&mut out, i, m.error.as_ref());
        }
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                continue;
            };
            let cb_params: Vec<String> = cb
                .params
                .iter()
                .map(|p| format!("{}: {}", js_param_name(&p.name), ts_type_for(&p.ty)))
                .collect();
            let register = js_fn_name(
                &m.path,
                &format!("register_{}", l.name),
                strip_module_prefix,
            );
            let unregister = js_fn_name(
                &m.path,
                &format!("unregister_{}", l.name),
                strip_module_prefix,
            );
            emit_doc(&mut out, &l.doc, "");
            out.push_str(&format!(
                "export function {register}(callback: ({}) => void): number\n",
                cb_params.join(", ")
            ));
            out.push_str(&format!("export function {unregister}(id: number): void\n"));
        }
        for f in &m.functions {
            let ts_name = js_fn_name(&m.path, &f.name, strip_module_prefix);
            emit_fn_doc(
                &mut out,
                &f.doc,
                &f.params,
                "",
                &ts_fn_tags(f, m.error.as_ref()),
            );
            out.push_str(&format!(
                "export function {}({}): {}\n",
                ts_name,
                ts_params(f),
                ts_ret(f)
            ));
        }
    }
    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "types.d.ts"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef, Module, Param,
        StructDef, StructField,
    };

    #[test]
    fn package_uses_optional_dependencies_per_platform() {
        use camino::Utf8Path;
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![make_module("calc")]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &NodeGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &NodeConfig::default(),
        )
        .expect("node supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        let main = files
            .iter()
            .find(|f| f.path.as_str().ends_with("node/package.json"))
            .expect("main package.json present");
        let FileContent::Text(pkg) = &main.content else {
            panic!("package.json is text");
        };
        assert!(pkg.contains("\"optionalDependencies\""));
        assert!(pkg.contains("weaveffi-darwin-arm64") && pkg.contains("weaveffi-win32-x64"));
        // The per-platform native package is gated by npm os/cpu.
        let plat = files
            .iter()
            .find(|f| {
                f.path
                    .as_str()
                    .ends_with("npm/weaveffi-win32-x64/package.json")
            })
            .expect("platform package present");
        let FileContent::Text(pp) = &plat.content else {
            panic!("platform package.json is text");
        };
        assert!(
            pp.contains("\"os\": [\"win32\"]") && pp.contains("\"cpu\": [\"x64\"]"),
            "os/cpu gating missing: {pp}"
        );
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn make_module(name: &str) -> Module {
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

    /// Test-only bridge from an inline [`Api`] literal to the model the
    /// production path receives from the driver.
    fn build_model(api: &Api) -> BindingModel {
        BindingModel::build(api, "weaveffi")
    }

    fn index_for(api: &Api, strip: bool) -> String {
        render_node_index(&build_model(api), strip, "weaveffi.yml")
    }

    fn dts_for(api: &Api, strip: bool) -> String {
        render_node_dts(&build_model(api), strip, "weaveffi.yml")
    }

    fn addon_for(api: &Api, strip: bool) -> String {
        render_addon_c(&build_model(api), strip, "weaveffi.yml")
    }

    #[test]
    fn listeners_generate_tsfn_register_unregister() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                doc: None,
                params: vec![Param {
                    name: "message".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        NodeGenerator
            .generate(&api, out, &NodeConfig::default())
            .unwrap();
        let addon = std::fs::read_to_string(dir.path().join("node/weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("napi_create_threadsafe_function"),
            "listeners must use threadsafe functions: {addon}"
        );
        assert!(
            addon.contains("Napi_weaveffi_events_register_message_listener"),
            "register N-API fn missing: {addon}"
        );
        assert!(
            addon.contains("Napi_weaveffi_events_unregister_message_listener"),
            "unregister N-API fn missing: {addon}"
        );
        assert!(
            addon.contains("napi_call_threadsafe_function(ctx->tsfn, p, napi_tsfn_nonblocking)"),
            "trampoline must queue payloads: {addon}"
        );
        assert!(
            addon.contains("napi_unref_threadsafe_function"),
            "tsfn must be unref'd so listeners don't pin the loop: {addon}"
        );
        let dts = std::fs::read_to_string(dir.path().join("node/types.d.ts")).unwrap();
        assert!(
            dts.contains(
                "export function registerMessageListener(callback: (message: string) => void): number"
            ),
            "register dts missing: {dts}"
        );
        assert!(
            dts.contains("export function unregisterMessageListener(id: number): void"),
            "unregister dts missing: {dts}"
        );
    }

    #[test]
    fn ts_type_for_primitives() {
        assert_eq!(ts_type_for(&TypeRef::I32), "number");
        assert_eq!(ts_type_for(&TypeRef::Bool), "boolean");
        assert_eq!(ts_type_for(&TypeRef::StringUtf8), "string");
        assert_eq!(ts_type_for(&TypeRef::Bytes), "Buffer");
        assert_eq!(ts_type_for(&TypeRef::Handle), "bigint");
    }

    #[test]
    fn ts_type_for_struct_and_enum() {
        assert_eq!(ts_type_for(&TypeRef::Record("Contact".into())), "Contact");
        assert_eq!(ts_type_for(&TypeRef::Enum("Color".into())), "Color");
        assert_eq!(
            ts_type_for(&TypeRef::TypedHandle("Contact".into())),
            "Contact"
        );
    }

    #[test]
    fn ts_type_for_cross_module_uses_local_name() {
        // A typed handle resolved to a parent-module struct (`kv.Store`) must
        // emit the bare local interface name, the only TS type in this module.
        assert_eq!(
            ts_type_for(&TypeRef::TypedHandle("kv.Store".into())),
            "Store"
        );
        assert_eq!(ts_type_for(&TypeRef::Record("kv.Store".into())), "Store");
        assert_eq!(ts_type_for(&TypeRef::Enum("kv.Kind".into())), "Kind");
    }

    #[test]
    fn ts_type_for_optional() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(ts_type_for(&ty), "string | null");
    }

    #[test]
    fn ts_type_for_list() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(ts_type_for(&ty), "number[]");
    }

    #[test]
    fn ts_type_for_list_of_optional() {
        let ty = TypeRef::List(Box::new(TypeRef::Optional(Box::new(TypeRef::I32))));
        assert_eq!(ts_type_for(&ty), "(number | null)[]");
    }

    #[test]
    fn ts_type_for_map() {
        let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        assert_eq!(ts_type_for(&ty), "Record<string, number>");
    }

    #[test]
    fn ts_type_for_optional_list() {
        let ty = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::I32))));
        assert_eq!(ts_type_for(&ty), "number[] | null");
    }

    #[test]
    fn generate_node_dts_with_structs() {
        let mut m = make_module("contacts");
        m.structs.push(StructDef {
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
                StructField {
                    name: "active".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                    default: None,
                },
            ],
            builder: false,
        });
        m.enums.push(EnumDef {
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
        });
        m.functions.push(Function {
            name: "get_contact".into(),
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
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        m.functions.push(Function {
            name: "list_contacts".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });

        let dts = dts_for(&make_api(vec![m]), true);

        assert!(dts.contains("export interface Contact {"));
        assert!(dts.contains("  name: string;"));
        assert!(dts.contains("  age: number;"));
        assert!(dts.contains("  active: boolean;"));
        assert!(dts.contains("export enum Color {"));
        assert!(dts.contains("  Red = 0,"));
        assert!(dts.contains("  Green = 1,"));
        assert!(dts.contains("  Blue = 2,"));
        assert!(dts.contains("export function getContact(id: number): Contact | null"));
        assert!(dts.contains("export function listContacts(): Contact[]"));

        let iface_pos = dts.find("export interface Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function getContact").unwrap();
        assert!(
            iface_pos < fn_pos,
            "interface should appear before functions"
        );
        assert!(enum_pos < fn_pos, "enum should appear before functions");
    }

    #[test]
    fn node_generates_binding_gyp() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_binding_gyp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator
            .generate(&api, out_dir, &NodeConfig::default())
            .unwrap();

        let gyp = std::fs::read_to_string(tmp.join("node").join("binding.gyp")).unwrap();
        assert!(
            gyp.contains("\"target_name\": \"weaveffi\""),
            "missing target_name: {gyp}"
        );
        assert!(
            gyp.contains("weaveffi_addon.c"),
            "missing source file: {gyp}"
        );

        let addon = std::fs::read_to_string(tmp.join("node").join("weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("napi_value Init("),
            "missing Init function: {addon}"
        );
        assert!(
            addon.contains("weaveffi_math_add"),
            "missing C ABI call: {addon}"
        );
        assert!(
            addon.contains("napi_get_cb_info"),
            "missing napi_get_cb_info call: {addon}"
        );

        let pkg = std::fs::read_to_string(tmp.join("node").join("package.json")).unwrap();
        assert!(pkg.contains("\"gypfile\": true"), "missing gypfile: {pkg}");
        assert!(
            pkg.contains("node-gyp rebuild"),
            "missing install script: {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_node_dts_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![
                Function {
                    name: "get_contact".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                        "Contact".into(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "set_favorite_color".to_string(),
                    params: vec![
                        Param {
                            name: "contact_id".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "color".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::Enum("Color".into()))),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_tags".to_string(),
                    params: vec![Param {
                        name: "contact_id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
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

        let tmp = std::env::temp_dir().join("weaveffi_test_node_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator
            .generate(
                &api,
                out_dir,
                &NodeConfig {
                    strip_module_prefix: true,
                    ..NodeConfig::default()
                },
            )
            .unwrap();

        let dts = std::fs::read_to_string(tmp.join("node").join("types.d.ts")).unwrap();

        assert!(
            dts.contains("export interface Contact {"),
            "missing Contact interface: {dts}"
        );
        assert!(dts.contains("  name: string;"), "missing name field: {dts}");
        assert!(
            dts.contains("  email: string | null;"),
            "missing optional email field: {dts}"
        );
        assert!(
            dts.contains("  tags: string[];"),
            "missing list tags field: {dts}"
        );

        assert!(
            dts.contains("export enum Color {"),
            "missing Color enum: {dts}"
        );
        assert!(dts.contains("  Red = 0,"), "missing Red variant: {dts}");
        assert!(dts.contains("  Green = 1,"), "missing Green variant: {dts}");
        assert!(dts.contains("  Blue = 2,"), "missing Blue variant: {dts}");

        assert!(
            dts.contains("export function getContact(id: number): Contact | null"),
            "missing getContact with optional return: {dts}"
        );
        assert!(
            dts.contains("export function listContacts(): Contact[]"),
            "missing listContacts with list return: {dts}"
        );
        assert!(
            dts.contains(
                "export function setFavoriteColor(contactId: number, color: Color | null): void"
            ),
            "missing setFavoriteColor with optional enum param: {dts}"
        );
        assert!(
            dts.contains("export function getTags(contactId: number): string[]"),
            "missing getTags with list return: {dts}"
        );

        let iface_pos = dts.find("export interface Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function getContact").unwrap();
        assert!(
            iface_pos < fn_pos,
            "interface should appear before functions"
        );
        assert!(enum_pos < fn_pos, "enum should appear before functions");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_custom_package_name() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_custom_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        let config = NodeConfig {
            package_name: Some("@myorg/cool-lib".into()),
            ..NodeConfig::default()
        };
        NodeGenerator.generate(&api, out_dir, &config).unwrap();

        let pkg = std::fs::read_to_string(tmp.join("node").join("package.json")).unwrap();
        assert!(
            pkg.contains("\"name\": \"@myorg/cool-lib\""),
            "package.json should use custom name: {pkg}"
        );
        assert!(
            !pkg.contains("\"name\": \"weaveffi\""),
            "package.json should not contain default name: {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_dts_has_jsdoc() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m.functions.push(Function {
                name: "subtract".into(),
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);

        let dts = dts_for(&api, true);

        assert!(
            dts.contains("Maps to C function: weaveffi_math_add"),
            "missing JSDoc for add: {dts}"
        );
        assert!(
            dts.contains("Maps to C function: weaveffi_math_subtract"),
            "missing JSDoc for subtract: {dts}"
        );
    }

    #[test]
    fn node_addon_has_no_todo() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            !addon.contains("// TODO: implement"),
            "generated addon.c should not contain TODO comments: {addon}"
        );
    }

    #[test]
    fn node_addon_extracts_args() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("napi_get_cb_info"),
            "generated addon.c should call napi_get_cb_info: {addon}"
        );
    }

    #[test]
    fn node_addon_frees_strings() {
        let api = make_api(vec![{
            let mut m = make_module("greet");
            m.functions.push(Function {
                name: "hello".into(),
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("weaveffi_free_string(result)"),
            "generated addon should free returned strings: {addon}"
        );
        assert!(
            addon.contains("#include <string.h>"),
            "generated addon should include string.h: {addon}"
        );
        assert!(
            addon.contains("#include <stdlib.h>"),
            "generated addon should include stdlib.h: {addon}"
        );
        assert!(
            addon.contains("weaveffi_error_clear(&err)"),
            "generated addon should clear errors: {addon}"
        );
    }

    #[test]
    fn node_custom_prefix_threads_to_user_symbols() {
        let api = make_api(vec![{
            let mut m = make_module("greet");
            m.functions.push(Function {
                name: "hello".into(),
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);

        let config = NodeConfig {
            prefix: Some("myffi".into()),
            ..NodeConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_node_custom_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        NodeGenerator.generate(&api, out_dir, &config).unwrap();

        // The output file name is a fixed library artifact name, not the ABI
        // prefix, so it stays `weaveffi_addon.c` regardless of `prefix`.
        let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();

        // User symbols pick up the configured ABI prefix.
        assert!(
            addon.contains("myffi_greet_hello"),
            "addon should call the prefixed user symbol myffi_greet_hello: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_greet_hello"),
            "addon must not emit the hard-coded weaveffi_ user symbol: {addon}"
        );
        assert!(
            addon.contains("#include \"myffi.h\""),
            "addon should include the prefixed header myffi.h: {addon}"
        );

        // Runtime ABI helpers are supplied by weaveffi-abi and stay literal.
        assert!(
            addon.contains("weaveffi_error"),
            "runtime weaveffi_error must remain literal: {addon}"
        );
        assert!(
            addon.contains("weaveffi_free_string"),
            "runtime weaveffi_free_string must remain literal: {addon}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_addon_checks_error() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("err.code"),
            "generated addon.c should check err.code: {addon}"
        );
    }

    #[test]
    fn node_strip_module_prefix() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);

        let config = NodeConfig {
            strip_module_prefix: true,
            ..NodeConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_node_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        NodeGenerator.generate(&api, out_dir, &config).unwrap();

        let dts = std::fs::read_to_string(tmp.join("node/types.d.ts")).unwrap();
        assert!(
            dts.contains("export function createContact("),
            "stripped name should be createContact: {dts}"
        );
        assert!(
            !dts.contains("export function contactsCreateContact("),
            "should not contain module-prefixed name: {dts}"
        );

        let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("\"createContact\""),
            "JS export name should be stripped: {addon}"
        );
        assert!(
            addon.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {addon}"
        );

        // Stripping is the default; `strip_module_prefix: false` restores
        // module-prefixed (still lowerCamelCase) names.
        let default_cfg = NodeConfig::default();
        assert!(
            default_cfg.strip_module_prefix,
            "stripping must be the default"
        );
        let no_strip = NodeConfig {
            strip_module_prefix: false,
            ..NodeConfig::default()
        };
        let tmp2 = std::env::temp_dir().join("weaveffi_test_node_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        NodeGenerator.generate(&api, out_dir2, &no_strip).unwrap();

        let dts2 = std::fs::read_to_string(tmp2.join("node/types.d.ts")).unwrap();
        assert!(
            dts2.contains("export function contactsCreateContact("),
            "opting out should restore module-prefixed names: {dts2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn node_typed_handle_type() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("contact: Contact"),
            "TypedHandle should use class type not bigint: {dts}"
        );
    }

    #[test]
    fn node_deeply_nested_optional() {
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
                r#async: false,
                cancellable: false,
                throws: false,
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
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("(Contact | null)[] | null"),
            "should contain deeply nested optional type: {dts}"
        );
    }

    #[test]
    fn node_map_of_lists() {
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
                throws: false,
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
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("Record<string, number[]>"),
            "should contain map of lists type: {dts}"
        );
    }

    #[test]
    fn node_enum_keyed_map() {
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
                r#async: false,
                cancellable: false,
                throws: false,
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
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("Record<Color, Contact>"),
            "should contain enum-keyed map type: {dts}"
        );
    }

    #[test]
    fn node_no_double_free_on_error() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("free(name)"),
            "malloc'd JS string copy should be freed after the C call: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_free_string(name)"),
            "input string param must not use weaveffi_free_string: {addon}"
        );
        let free_pos = addon
            .find("free(name)")
            .expect("free(name) should be present");
        let err_pos = addon
            .find("if (err.code != 0)")
            .expect("err.code check should be present");
        assert!(
            free_pos < err_pos,
            "cleanup should run before error check: free at {free_pos}, err at {err_pos}"
        );
        let err_block_start = addon
            .find("  if (err.code != 0) {\n")
            .expect("error if block should be present");
        let after_err = &addon[err_block_start..];
        let err_block_end_rel = after_err
            .find("  }\n  napi_value ret;")
            .expect("napi_value ret should follow error block");
        let err_block = &addon[err_block_start..err_block_start + err_block_end_rel];
        assert!(
            !err_block.contains("result"),
            "error path should not touch result before return NULL: {err_block}"
        );
    }

    #[test]
    fn node_null_check_on_optional_return() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
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
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("if (result == NULL)"),
            "optional struct return should null-check before wrapping: {addon}"
        );
        assert!(
            addon.contains("napi_get_null"),
            "optional absent should return JS null via napi_get_null: {addon}"
        );
    }

    #[test]
    fn node_async_returns_promise() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.functions.push(Function {
                name: "run".into(),
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
                throws: false,
                deprecated: None,
                since: None,
            });
            m.functions.push(Function {
                name: "fire_and_forget".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("Promise<"),
            "async function should return Promise in .d.ts: {dts}"
        );
        assert!(
            dts.contains("): Promise<string>"),
            "async string return should be Promise<string>: {dts}"
        );
        assert!(
            dts.contains("): Promise<void>"),
            "async void return should be Promise<void>: {dts}"
        );
    }

    #[test]
    fn node_addon_creates_promise() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.functions.push(Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("napi_create_promise"),
            "async addon should call napi_create_promise: {addon}"
        );
        assert!(
            addon.contains("napi_resolve_deferred"),
            "async callback should call napi_resolve_deferred: {addon}"
        );
        assert!(
            addon.contains("napi_reject_deferred"),
            "async callback should call napi_reject_deferred: {addon}"
        );
        assert!(
            addon.contains("weaveffi_tasks_run_napi_actx"),
            "async addon should define per-fn async context struct: {addon}"
        );
        assert!(
            addon.contains("weaveffi_tasks_run_async("),
            "async addon should call the _async C function: {addon}"
        );
        assert!(
            addon.contains("weaveffi_tasks_run_napi_cb"),
            "async addon should define the callback: {addon}"
        );
        // The completion callback may fire on any producer thread, so it must
        // queue through a threadsafe function instead of touching napi_env.
        assert!(
            addon.contains("napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking)"),
            "completion callback must hop to the JS thread via tsfn: {addon}"
        );
        assert!(
            !addon.contains("napi_resolve_deferred(ctx->env"),
            "deferred must never be settled from the producer thread: {addon}"
        );
    }

    /// The N-API deferred is created with `napi_create_promise` and settled
    /// (on the JS thread) by exactly one of `napi_resolve_deferred` /
    /// `napi_reject_deferred`. The per-fn async context that carries the
    /// deferred + threadsafe function across threads must be allocated once
    /// and freed exactly once, and the tsfn released exactly once.
    #[test]
    fn node_async_pins_callback_for_lifetime() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.functions.push(Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = addon_for(&api, true);
        let create_count = addon.matches("napi_create_promise").count();
        let resolve_count = addon.matches("napi_resolve_deferred").count();
        let reject_count = addon.matches("napi_reject_deferred").count();
        let alloc_count = addon
            .matches("calloc(1, sizeof(weaveffi_tasks_run_napi_actx))")
            .count();
        let free_count = addon.matches("free(ctx);").count();
        let release_count = addon
            .matches("napi_release_threadsafe_function(ctx->tsfn, napi_tsfn_release);")
            .count();
        assert_eq!(
            create_count, 1,
            "expected one napi_create_promise per async fn, got {create_count}: {addon}"
        );
        assert_eq!(
            resolve_count, 1,
            "expected one napi_resolve_deferred per async fn, got {resolve_count}: {addon}"
        );
        assert_eq!(
            reject_count, 1,
            "expected one napi_reject_deferred per async fn, got {reject_count}: {addon}"
        );
        assert_eq!(
            alloc_count, free_count,
            "ctx alloc / free must balance per async fn: alloc={alloc_count} free={free_count}: {addon}"
        );
        assert_eq!(
            release_count, 1,
            "tsfn must be released exactly once per async fn, got {release_count}: {addon}"
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
                throws: false,
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
    fn node_emits_doc_on_function() {
        let dts = dts_for(&make_api(vec![doc_module()]), true);
        assert!(dts.contains("Performs a thing."), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_struct() {
        let dts = dts_for(&make_api(vec![doc_module()]), true);
        assert!(dts.contains("/** An item we track. */"), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_enum_variant() {
        let dts = dts_for(&make_api(vec![doc_module()]), true);
        assert!(dts.contains("/** Kind of item. */"), "{dts}");
        assert!(dts.contains("/** A small one */"), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_field() {
        let dts = dts_for(&make_api(vec![doc_module()]), true);
        assert!(dts.contains("/** Stable id */"), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_param() {
        let dts = dts_for(&make_api(vec![doc_module()]), true);
        assert!(dts.contains("@param x the input value"), "{dts}");
    }

    // --- Rich (algebraic) enum support ------------------------------------

    /// A module mirroring `samples/shapes/shapes.yml`: a rich enum `Shape`
    /// (unit + f64 + two-f32 + string/u8 variants), a plain enum `Channel`, and
    /// the free functions that take/return the rich enum plus a numeric smoke.
    fn shapes_module() -> Module {
        fn field(name: &str, ty: TypeRef) -> StructField {
            StructField {
                name: name.into(),
                ty,
                doc: None,
                default: None,
            }
        }
        fn variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
            EnumVariant {
                name: name.into(),
                value,
                doc: None,
                fields,
            }
        }
        Module {
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
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![
                EnumDef {
                    name: "Shape".into(),
                    doc: None,
                    variants: vec![
                        variant("Empty", 0, vec![]),
                        variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
                        variant(
                            "Rectangle",
                            2,
                            vec![field("width", TypeRef::F32), field("height", TypeRef::F32)],
                        ),
                        variant(
                            "Labeled",
                            3,
                            vec![
                                field("label", TypeRef::StringUtf8),
                                field("count", TypeRef::U8),
                            ],
                        ),
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
        }
    }

    #[test]
    fn rich_enum_addon_exposes_native_helpers() {
        let addon = addon_for(&make_api(vec![shapes_module()]), false);

        // Tag reader, per-variant constructors, per-variant field getters, and
        // the destructor are all defined as native functions over the C ABI.
        for sym in [
            "Napi_weaveffi_shapes_Shape_tag",
            "Napi_weaveffi_shapes_Shape_Empty_new",
            "Napi_weaveffi_shapes_Shape_Circle_new",
            "Napi_weaveffi_shapes_Shape_Rectangle_new",
            "Napi_weaveffi_shapes_Shape_Labeled_new",
            "Napi_weaveffi_shapes_Shape_Circle_get_radius",
            "Napi_weaveffi_shapes_Shape_Rectangle_get_width",
            "Napi_weaveffi_shapes_Shape_Rectangle_get_height",
            "Napi_weaveffi_shapes_Shape_Labeled_get_label",
            "Napi_weaveffi_shapes_Shape_Labeled_get_count",
            "Napi_weaveffi_shapes_Shape_destroy",
        ] {
            assert!(addon.contains(sym), "missing native helper {sym}: {addon}");
        }

        // Each is exported under an idiomatic JS name.
        for js in [
            "\"shapes_Shape_tag\"",
            "\"shapes_Shape_empty_new\"",
            "\"shapes_Shape_circle_new\"",
            "\"shapes_Shape_rectangle_new\"",
            "\"shapes_Shape_labeled_new\"",
            "\"shapes_Shape_circle_get_radius\"",
            "\"shapes_Shape_labeled_get_label\"",
            "\"shapes_Shape_labeled_get_count\"",
            "\"shapes_Shape_destroy\"",
        ] {
            assert!(addon.contains(js), "missing JS export {js}: {addon}");
        }
    }

    #[test]
    fn rich_enum_addon_calls_c_abi_correctly() {
        let addon = addon_for(&make_api(vec![shapes_module()]), false);

        // Constructors thread out_err and return the owned pointer as a handle.
        assert!(
            addon.contains(
                "weaveffi_shapes_Shape* result = weaveffi_shapes_Shape_Circle_new(radius, &err);"
            ),
            "circle ctor must call the C constructor: {addon}"
        );
        // f32 variant fields narrow from the N-API double getter.
        assert!(
            addon.contains(
                "weaveffi_shapes_Shape_Rectangle_new((float)width_raw, (float)height_raw, &err);"
            ),
            "rectangle ctor must narrow f32 args: {addon}"
        );
        // string + u8 variant: string copy freed after the call, u8 narrowed.
        assert!(
            addon.contains("weaveffi_shapes_Shape_Labeled_new(label, (uint8_t)count_raw, &err);"),
            "labeled ctor must marshal string + u8: {addon}"
        );
        assert!(
            addon.contains("free(label);"),
            "labeled ctor must free its string copy: {addon}"
        );
        // tag reader returns the int32 discriminant.
        assert!(
            addon.contains("napi_create_int32(env, weaveffi_shapes_Shape_tag(self), &ret);"),
            "tag reader must return the discriminant: {addon}"
        );
        // String getter frees the owned C string after copying it to JS.
        assert!(
            addon.contains("weaveffi_free_string(ret_s);"),
            "string field getter must free the owned C string: {addon}"
        );
        // Destructor frees the opaque object.
        assert!(
            addon.contains("weaveffi_shapes_Shape_destroy(self);"),
            "destructor must free the object: {addon}"
        );

        // Free functions marshal the rich enum as the opaque handle (no attempt
        // to materialize it as a plain object), in and out.
        assert!(
            addon.contains(
                "weaveffi_shapes_describe((const weaveffi_shapes_Shape*)(intptr_t)shape_raw, &err);"
            ),
            "describe must pass the opaque handle: {addon}"
        );
        assert!(
            addon.contains(
                "weaveffi_shapes_Shape* result = weaveffi_shapes_scale((const weaveffi_shapes_Shape*)(intptr_t)shape_raw, factor, &err);"
            ),
            "scale must take and return the opaque handle: {addon}"
        );
        assert!(
            addon.contains("napi_create_int64(env, (int64_t)(intptr_t)result, &ret);"),
            "scale must return the opaque handle as int64: {addon}"
        );
    }

    #[test]
    fn rich_enum_index_js_exposes_class() {
        let index = index_for(&make_api(vec![shapes_module()]), false);

        assert!(
            index.contains("class Shape {"),
            "missing Shape class: {index}"
        );
        // Per-variant static factories.
        for factory in [
            "static empty() {",
            "static circle(radius) {",
            "static rectangle(width, height) {",
            "static labeled(label, count) {",
        ] {
            assert!(
                index.contains(factory),
                "missing factory `{factory}`: {index}"
            );
        }
        // The factories call the native constructors through the generic
        // rebranding helper (a failure is marshalling or a panic).
        assert!(
            index.contains(
                "return new Shape(__invoke(addon.shapes_Shape_circle_new, [radius], __generic));"
            ),
            "circle factory must call the native ctor: {index}"
        );
        // tag reader + namespaced per-variant getters.
        assert!(
            index.contains("tag() {") && index.contains("addon.shapes_Shape_tag(this._handle)"),
            "missing tag(): {index}"
        );
        for getter in [
            "get circleRadius() {",
            "get rectangleWidth() {",
            "get rectangleHeight() {",
            "get labeledLabel() {",
            "get labeledCount() {",
        ] {
            assert!(index.contains(getter), "missing getter `{getter}`: {index}");
        }
        // Cleanup: explicit destroy + a FinalizationRegistry safety net.
        assert!(
            index.contains("destroy() {")
                && index.contains("addon.shapes_Shape_destroy(this._handle)"),
            "missing destroy(): {index}"
        );
        assert!(
            index.contains("new FinalizationRegistry"),
            "missing FinalizationRegistry cleanup: {index}"
        );
        // Discriminant map.
        assert!(
            index.contains(
                "Shape.Tag = Object.freeze({ Empty: 0, Circle: 1, Rectangle: 2, Labeled: 3 });"
            ),
            "missing Tag discriminant map: {index}"
        );
        // Module functions that carry the rich enum are rewrapped to speak the
        // class; `scale` returns a wrapped instance, `describe` unwraps its arg.
        assert!(
            index.contains("wv.shapesScale = function (shape, factor) {")
                && index.contains(
                    "__invoke(addon.shapesScale, [shape instanceof Shape ? shape._handle : shape, factor], __generic)"
                )
                && index.contains("return new Shape(_r);"),
            "scale must be rewrapped to return a Shape: {index}"
        );
        assert!(
            index.contains(
                "return __invoke(addon.shapesDescribe, [shape instanceof Shape ? shape._handle : shape], __generic);"
            ),
            "describe must unwrap a Shape argument: {index}"
        );
    }

    #[test]
    fn index_js_without_domains_wraps_with_generic_brand() {
        // Even with no rich enums, interfaces, or error domains, every
        // function gets a wrapper so a non-zero error slot (panic or
        // marshalling failure) surfaces as the generic brand class.
        let mut m = make_module("math");
        m.functions.push(Function {
            name: "add".into(),
            params: vec![Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        let index = index_for(&make_api(vec![m]), false);
        assert!(
            index.contains("class WeaveFFIError extends Error {"),
            "generic brand class missing: {index}"
        );
        assert!(
            index.contains("wv.mathAdd = function (a) {")
                && index.contains("return __invoke(addon.mathAdd, [a], __generic);"),
            "non-throwing fn must wrap through the generic brand: {index}"
        );
        assert!(
            index.contains("module.exports = wv;"),
            "index must export the wrapper namespace: {index}"
        );
    }

    #[test]
    fn rich_enum_dts_emits_class_not_enum() {
        let dts = dts_for(&make_api(vec![shapes_module()]), false);

        // Rich enum -> class with factories, tag(), getters, destroy().
        assert!(
            dts.contains("export class Shape {"),
            "rich enum must be a class: {dts}"
        );
        assert!(
            !dts.contains("export enum Shape"),
            "rich enum must not be a plain enum: {dts}"
        );
        assert!(
            dts.contains("static circle(radius: number): Shape;"),
            "{dts}"
        );
        assert!(
            dts.contains("static labeled(label: string, count: number): Shape;"),
            "{dts}"
        );
        assert!(dts.contains("tag(): number;"), "{dts}");
        assert!(dts.contains("get circleRadius(): number;"), "{dts}");
        assert!(dts.contains("get labeledLabel(): string;"), "{dts}");
        assert!(dts.contains("destroy(): void;"), "{dts}");

        // Plain enum still surfaces as a numeric `enum`.
        assert!(
            dts.contains("export enum Channel {"),
            "plain enum stays an enum: {dts}"
        );

        // Free functions are typed in terms of the class; unstripped names
        // keep the module prefix but are still lowerCamelCase.
        assert!(
            dts.contains("export function shapesDescribe(shape: Shape): string"),
            "{dts}"
        );
        assert!(
            dts.contains("export function shapesScale(shape: Shape, factor: number): Shape"),
            "{dts}"
        );
    }

    // --- Interfaces and typed errors ----------------------------------------

    /// A module mirroring the kvstore sample's shape: a `KvError` domain, a
    /// `Store` interface (canonical `new` + non-throwing factory + throwing
    /// and non-throwing methods + an async method + a static), and free
    /// functions exercising the throws split and interface params/returns.
    fn kv_module() -> Module {
        fn param(name: &str, ty: TypeRef) -> Param {
            Param {
                name: name.into(),
                ty,
                mutable: false,
                doc: None,
            }
        }
        fn func(
            name: &str,
            params: Vec<Param>,
            returns: Option<TypeRef>,
            throws: bool,
        ) -> Function {
            Function {
                name: name.into(),
                params,
                returns,
                doc: None,
                r#async: false,
                cancellable: false,
                throws,
                deprecated: None,
                since: None,
            }
        }
        Module {
            name: "kv".into(),
            functions: vec![
                func("ping", vec![], Some(TypeRef::Bool), false),
                func(
                    "clone_store",
                    vec![param("source_store", TypeRef::Interface("Store".into()))],
                    Some(TypeRef::Interface("Store".into())),
                    true,
                ),
            ],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: Some("A key-value store.".into()),
                constructors: vec![
                    func("new", vec![param("path", TypeRef::StringUtf8)], None, true),
                    func(
                        "open_readonly",
                        vec![param("path", TypeRef::StringUtf8)],
                        None,
                        false,
                    ),
                ],
                methods: vec![
                    func(
                        "put",
                        vec![
                            param("key", TypeRef::StringUtf8),
                            param("the_value", TypeRef::StringUtf8),
                        ],
                        None,
                        true,
                    ),
                    func("count", vec![], Some(TypeRef::I64), false),
                    func(
                        "list_keys",
                        vec![param(
                            "prefix",
                            TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        )],
                        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                        true,
                    ),
                    Function {
                        name: "compact".into(),
                        params: vec![],
                        returns: Some(TypeRef::I64),
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        throws: true,
                        deprecated: None,
                        since: None,
                    },
                ],
                statics: vec![func("default_capacity", vec![], Some(TypeRef::I64), false)],
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    ErrorCode {
                        name: "KeyNotFound".into(),
                        code: 1001,
                        message: "key not found".into(),
                        doc: Some("The requested key does not exist.".into()),
                    },
                    ErrorCode {
                        name: "StoreFull".into(),
                        code: 1003,
                        message: "store is full".into(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }
    }

    #[test]
    fn interface_addon_exposes_member_entry_points() {
        let addon = addon_for(&make_api(vec![kv_module()]), true);

        // One native entry point per member plus the destructor, all named
        // from the model's `{c_tag}_{member}` symbols.
        for sym in [
            "static napi_value Napi_weaveffi_kv_Store_new(",
            "static napi_value Napi_weaveffi_kv_Store_open_readonly(",
            "static napi_value Napi_weaveffi_kv_Store_put(",
            "static napi_value Napi_weaveffi_kv_Store_count(",
            "static napi_value Napi_weaveffi_kv_Store_compact(",
            "static napi_value Napi_weaveffi_kv_Store_default_capacity(",
            "static napi_value Napi_weaveffi_kv_Store_destroy(",
        ] {
            assert!(addon.contains(sym), "missing entry point {sym}: {addon}");
        }

        // Constructors return the owned object pointer as an int64 handle.
        assert!(
            addon.contains("weaveffi_kv_Store* result = weaveffi_kv_Store_new(path, &err);"),
            "ctor must call the C constructor: {addon}"
        );
        // Methods read the wrapped pointer from args[0] and pass it as the
        // leading C argument, ahead of the logical parameters.
        assert!(
            addon.contains(
                "weaveffi_kv_Store_put((const weaveffi_kv_Store*)(intptr_t)self_raw, key, the_value, &err);"
            ),
            "method must pass self first: {addon}"
        );
        // The async launcher symbol comes from the model (member base plus
        // `_async`), with the self slot leading.
        assert!(
            addon.contains("weaveffi_kv_Store_compact_async((const weaveffi_kv_Store*)(intptr_t)self_raw, weaveffi_kv_Store_compact_napi_cb, ctx);"),
            "async method must call the model's launcher with self: {addon}"
        );
        // The destructor frees the object.
        assert!(
            addon.contains("weaveffi_kv_Store_destroy(self);"),
            "destroy must free the object: {addon}"
        );

        // Members export under stripped, interface-scoped JS names.
        for js in [
            "\"Store_new\"",
            "\"Store_open_readonly\"",
            "\"Store_put\"",
            "\"Store_default_capacity\"",
            "\"Store_destroy\"",
        ] {
            assert!(addon.contains(js), "missing JS export {js}: {addon}");
        }

        // Every failure path throws the code-carrying error object.
        assert!(
            addon.contains(
                "napi_throw(env, weaveffi_napi_error_value(env, err.code, err.message));"
            ),
            "sync errors must carry the ABI code: {addon}"
        );
        assert!(
            addon.contains("napi_set_named_property(env, err, \"code\", code_val);"),
            "the error helper must attach the numeric code: {addon}"
        );
    }

    #[test]
    fn iterator_addon_is_lazy() {
        let addon = addon_for(&make_api(vec![kv_module()]), true);

        // The launch entry point never drains: it boxes the owned handle
        // into a state cell and wraps it in an external with a finalizer.
        assert!(
            !addon.contains("while (weaveffi_kv_Store_ListKeysIterator_next"),
            "the addon must not drain the iterator into an array: {addon}"
        );
        assert!(
            addon.contains(
                "weaveffi_napi_iter_state* iter_state = (weaveffi_napi_iter_state*)calloc(1, sizeof(weaveffi_napi_iter_state));"
            ),
            "launch must box the handle into a state cell: {addon}"
        );
        assert!(
            addon.contains(
                "napi_create_external(env, iter_state, weaveffi_kv_Store_list_keys_napi_iter_finalize, NULL, &ret);"
            ),
            "launch must wrap the cell in an external with a finalizer: {addon}"
        );

        // Per-iterator `next` and `destroy` entry points hang off the model's
        // iterator-tag symbols and export under the wrapper's addon name.
        assert!(
            addon.contains(
                "static napi_value Napi_weaveffi_kv_Store_ListKeysIterator_next(napi_env env, napi_callback_info info) {"
            ),
            "missing the per-iterator next entry point: {addon}"
        );
        assert!(
            addon.contains(
                "static napi_value Napi_weaveffi_kv_Store_ListKeysIterator_destroy(napi_env env, napi_callback_info info) {"
            ),
            "missing the per-iterator destroy entry point: {addon}"
        );
        assert!(
            addon.contains("\"Store_list_keys_iterNext\"")
                && addon.contains("\"Store_list_keys_iterDestroy\""),
            "next/destroy must export under the wrapper's addon names: {addon}"
        );

        // One producer pull per call, threading the per-step error slot.
        assert!(
            addon.contains(
                "if (!weaveffi_kv_Store_ListKeysIterator_next((weaveffi_kv_Store_ListKeysIterator*)state->iter, &iter_item, &iter_err)) {"
            ),
            "next must issue exactly one producer pull with the error slot: {addon}"
        );
        // A per-step fault throws the code-carrying error (list_keys is
        // `throws`, so the JS layer maps it to the domain class).
        assert!(
            addon.contains(
                "napi_throw(env, weaveffi_napi_error_value(env, iter_err.code, iter_err.message));"
            ),
            "next must throw the per-step error: {addon}"
        );
        // Each yielded string element is freed after the JS string exists.
        let convert = addon
            .find("napi_create_string_utf8(env, iter_item ? iter_item : \"\", NAPI_AUTO_LENGTH, &ret);")
            .expect("next must convert the yielded element");
        let free = addon
            .find("weaveffi_free_string((char*)iter_item);")
            .expect("next must free the yielded string");
        assert!(
            convert < free,
            "the element must be converted before it is freed: {addon}"
        );

        // Every destroy site nulls the cell first, so exhaustion, explicit
        // destroy, and the finalizer never double-free.
        assert!(
            addon.contains(
                "weaveffi_kv_Store_ListKeysIterator_destroy((weaveffi_kv_Store_ListKeysIterator*)state->iter);"
            ),
            "destroy must release through the state cell: {addon}"
        );
        assert!(
            addon.contains("if (state != NULL && state->iter != NULL) {"),
            "explicit destroy must guard against double-destroy: {addon}"
        );
        assert!(
            addon.contains(
                "static void weaveffi_kv_Store_list_keys_napi_iter_finalize(napi_env env, void* data, void* hint) {"
            ),
            "abandoned iterators must be reclaimed by a finalizer: {addon}"
        );
    }

    #[test]
    fn iterator_js_class_implements_protocol() {
        let index = index_for(&make_api(vec![kv_module()]), true);

        // The shared class implements the iterator protocol lazily.
        assert!(
            index.contains("class WeaveFFIIterator {"),
            "missing the shared iterator class: {index}"
        );
        assert!(
            index.contains("[Symbol.iterator]() {"),
            "the class must be iterable: {index}"
        );
        assert!(
            index.contains("return(value) {"),
            "the class must clean up on early exit: {index}"
        );
        // One native pull per step, routed through the rebranding helper.
        assert!(
            index.contains("const _v = __invoke(this._nextFn, [this._ext], this._map);"),
            "next() must issue one native pull: {index}"
        );
        // Early exit destroys the native handle exactly once.
        assert!(
            index.contains("this._destroyFn(this._ext);"),
            "return() must destroy the native handle: {index}"
        );

        // The method wrapper launches, then hands the external to the class
        // with its per-iterator next/destroy bindings and error mapping.
        assert!(
            index.contains(
                "const _it = __invoke(addon.Store_list_keys, [this._handle, prefix], __kvErrorFrom);"
            ),
            "the wrapper must launch the native iterator: {index}"
        );
        assert!(
            index.contains(
                "return new WeaveFFIIterator(_it, addon.Store_list_keys_iterNext, addon.Store_list_keys_iterDestroy, __kvErrorFrom, null);"
            ),
            "the wrapper must return the lazy iterator: {index}"
        );
    }

    #[test]
    fn iterator_dts_is_iterable_iterator() {
        let dts = dts_for(&make_api(vec![kv_module()]), true);
        assert!(
            dts.contains("IterableIterator<string>"),
            "iter<string> must surface as IterableIterator<string>: {dts}"
        );
        assert!(
            !dts.contains("string[]"),
            "iter<T> must not surface as an array: {dts}"
        );
    }

    #[test]
    fn interface_index_js_class() {
        let index = index_for(&make_api(vec![kv_module()]), true);

        assert!(
            index.contains("class Store {"),
            "missing Store class: {index}"
        );
        // The canonical `new` constructor maps to the JS constructor and
        // routes failures through the domain factory (it throws).
        assert!(
            index.contains("constructor(path) {")
                && index
                    .contains("this._handle = __invoke(addon.Store_new, [path], __kvErrorFrom);"),
            "missing canonical constructor: {index}"
        );
        // Other constructors become static factories; this one does not
        // throw, so failures rebrand as the generic class.
        assert!(
            index.contains("static openReadonly(path) {")
                && index.contains("__invoke(addon.Store_open_readonly, [path], __generic)")
                && index.contains("return Store._fromHandle(_r);"),
            "missing factory wrapping the owned handle: {index}"
        );
        // Methods pass the wrapped handle as the leading argument.
        assert!(
            index.contains("put(key, theValue) {")
                && index.contains(
                    "return __invoke(addon.Store_put, [this._handle, key, theValue], __kvErrorFrom);"
                ),
            "missing method with leading self handle: {index}"
        );
        // The async method rejects typed (it throws).
        assert!(
            index.contains("compact() {")
                && index.contains(
                    "return __invokeAsync(addon.Store_compact, [this._handle], __kvErrorFrom);"
                ),
            "missing async method: {index}"
        );
        // Statics are static methods.
        assert!(
            index.contains("static defaultCapacity() {")
                && index.contains("return __invoke(addon.Store_default_capacity, [], __generic);"),
            "missing static method: {index}"
        );
        // Disposal follows the opaque-wrapper idiom: explicit destroy plus a
        // FinalizationRegistry safety net calling the destroy export.
        assert!(
            index.contains("destroy() {") && index.contains("addon.Store_destroy(this._handle);"),
            "missing destroy(): {index}"
        );
        assert!(
            index.contains("Store._cleanup = new FinalizationRegistry"),
            "missing FinalizationRegistry: {index}"
        );

        // A free function borrowing an interface unwraps the class argument
        // and wraps the owned returned handle in a new instance.
        assert!(
            index.contains("wv.cloneStore = function (sourceStore) {")
                && index.contains(
                    "__invoke(addon.cloneStore, [sourceStore instanceof Store ? sourceStore._handle : sourceStore], __kvErrorFrom)"
                )
                && index.contains("return Store._fromHandle(_r);"),
            "interface param/return must cross as instances: {index}"
        );
    }

    #[test]
    fn typed_error_classes_js() {
        let index = index_for(&make_api(vec![kv_module()]), true);

        // Domain class extends the generic brand; per-code subclasses carry
        // their stable CODE and default message.
        assert!(
            index.contains("class KvError extends WeaveFFIError {"),
            "missing domain class: {index}"
        );
        assert!(
            index.contains("class KeyNotFoundError extends KvError {"),
            "missing per-code class: {index}"
        );
        assert!(
            index.contains("KeyNotFoundError.CODE = 1001;")
                && index.contains("StoreFullError.CODE = 1003;"),
            "missing stable code constants: {index}"
        );
        assert!(
            index.contains("super(1001, message || 'key not found');"),
            "per-code class must default its message: {index}"
        );
        // The factory maps a raw code to the matching class and falls back to
        // the generic brand for unknown codes (panics, marshalling).
        assert!(
            index.contains("function __kvErrorFrom(code, message) {"),
            "missing domain factory: {index}"
        );
        assert!(
            index.contains("1001: KeyNotFoundError, 1003: StoreFullError"),
            "missing code table: {index}"
        );
        assert!(
            index.contains(
                "return _cls === undefined ? new WeaveFFIError(code, message) : new _cls(message);"
            ),
            "factory must fall back to the generic brand: {index}"
        );
        // Both surfaces are exported.
        assert!(
            index.contains("wv.KvError = KvError;")
                && index.contains("wv.KeyNotFoundError = KeyNotFoundError;"),
            "error classes must be exported: {index}"
        );
    }

    #[test]
    fn throws_split_picks_the_error_surface() {
        let index = index_for(&make_api(vec![kv_module()]), true);

        // throws == false: plain wrapper; a non-zero error slot (panic or
        // marshalling failure only) still rebrands as the generic class.
        assert!(
            index.contains("wv.ping = function () {")
                && index.contains("return __invoke(addon.ping, [], __generic);"),
            "non-throwing fn must use the generic map: {index}"
        );
        // throws == true: failures map through the module's domain factory.
        assert!(
            index.contains("__invoke(addon.cloneStore, [sourceStore instanceof Store ? sourceStore._handle : sourceStore], __kvErrorFrom)"),
            "throwing fn must use the domain map: {index}"
        );
    }

    #[test]
    fn typed_error_and_interface_dts() {
        let dts = dts_for(&make_api(vec![kv_module()]), true);

        // The generic brand plus the domain surface.
        assert!(
            dts.contains("export class WeaveFFIError extends Error {"),
            "missing generic brand: {dts}"
        );
        assert!(
            dts.contains("export class KvError extends WeaveFFIError {"),
            "missing domain class: {dts}"
        );
        assert!(
            dts.contains("export class KeyNotFoundError extends KvError {")
                && dts.contains("static readonly CODE: 1001;"),
            "missing per-code class: {dts}"
        );

        // The interface class mirrors the JS surface.
        assert!(
            dts.contains("export class Store {"),
            "missing Store class: {dts}"
        );
        assert!(
            dts.contains("constructor(path: string);"),
            "missing canonical constructor: {dts}"
        );
        assert!(
            dts.contains("static openReadonly(path: string): Store;"),
            "missing factory: {dts}"
        );
        assert!(
            dts.contains("put(key: string, theValue: string): void;"),
            "missing method with camel params: {dts}"
        );
        assert!(
            dts.contains("compact(): Promise<number>;"),
            "missing async method: {dts}"
        );
        assert!(
            dts.contains("static defaultCapacity(): number;"),
            "missing static: {dts}"
        );
        assert!(dts.contains("destroy(): void;"), "missing destroy: {dts}");

        // Throwing callables document their domain; interface params and
        // returns are typed as the class.
        assert!(
            dts.contains("@throws {KvError}"),
            "missing @throws tag: {dts}"
        );
        assert!(
            dts.contains("export function cloneStore(sourceStore: Store): Store"),
            "missing interface-typed free function: {dts}"
        );
        assert!(
            dts.contains("export function ping(): boolean"),
            "missing plain function: {dts}"
        );
    }
}
