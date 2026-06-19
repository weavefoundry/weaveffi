//! Node.js (N-API) binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader plus TypeScript type definitions for the
//! companion N-API addon. Async functions surface as `Promise`-returning
//! methods. Implements [`LanguageBackend`]; the shared driver bridges it into
//! the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToSnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::model::{
    BindingModel, CallbackBinding, EnumBinding, FnBinding, ListenerBinding, ParamBinding,
    StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_json_prelude, render_prelude, render_trailer,
    wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`NodeGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// npm package name (default `"weaveffi"`).
    pub package_name: Option<String>,
    /// When `true`, strip the IR module name prefix from emitted
    /// JS/TS function names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the native addon calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
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
        _model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let prefix = config.prefix();
        let strip = config.strip_module_prefix;
        vec![
            OutputFile::new(
                dir.join("index.js"),
                render_node_index(api, prefix, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("types.d.ts"),
                render_node_dts(api, prefix, strip, input_basename),
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
                render_addon_c(api, prefix, strip, input_basename),
            ),
        ]
    }

    fn package(
        &self,
        api: &Api,
        _model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let prefix = config.prefix();
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
            .map(|p| (p, format!("{}-{}-{}", package.name, p.node_os(), p.node_cpu())))
            .collect();

        let mut files = vec![
            PackagedFile::text(
                dir.join("index.js"),
                render_node_index(api, prefix, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("types.d.ts"),
                render_node_dts(api, prefix, strip, input_basename),
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
                render_addon_c(api, prefix, strip, input_basename),
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

fn is_c_ptr_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::Bytes
            | TypeRef::Struct(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Iterator(_)
    )
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
        TypeRef::Struct(s) => format!("{}*", c_abi_struct_name(s, module, prefix)),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            c_elem_type(inner, module, prefix)
        }
        TypeRef::Map(_, _) => "void*".into(),
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
        TypeRef::Struct(s) => format!("{}*", c_abi_struct_name(s, module, prefix)),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) => {
            if is_c_ptr_type(inner) {
                c_ret_type_str(inner, module, prefix)
            } else {
                format!("{}*", c_elem_type(inner, module, prefix))
            }
        }
        TypeRef::List(inner) => format!("{}*", c_elem_type(inner, module, prefix)),
        TypeRef::Map(_, _) => "void".into(),
        TypeRef::Iterator(_) => "void*".into(),
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
        | TypeRef::Struct(_) => "napi_get_value_int64",
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

fn render_addon_c(
    api: &Api,
    prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str(&format!(
        "#include <node_api.h>\n#include \"{prefix}.h\"\n#include <stdlib.h>\n#include <string.h>\n\n"
    ));

    let model = BindingModel::build(api, prefix);
    let mut all_exports: Vec<(String, String)> = Vec::new();
    let structs = struct_registry(&model);

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
                wrapper_name(
                    &m.path,
                    &format!("register_{}", l.name),
                    strip_module_prefix,
                ),
                format!("Napi_{}", l.register_symbol),
            ));
            all_exports.push((
                wrapper_name(
                    &m.path,
                    &format!("unregister_{}", l.name),
                    strip_module_prefix,
                ),
                format!("Napi_{}", l.unregister_symbol),
            ));
        }
        for f in &m.functions {
            let c_name = &f.c_base;
            let napi_name = format!("Napi_{c_name}");
            let js_name = wrapper_name(&m.path, &f.name, strip_module_prefix);
            all_exports.push((js_name, napi_name.clone()));

            if f.is_async {
                render_async_machinery(&mut out, f, c_name, &m.path, prefix, &structs);
            }

            out.push_str(&format!(
                "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
            ));
            if f.is_async {
                render_async_napi_body(&mut out, f, c_name, &m.path, prefix);
            } else {
                render_napi_body(&mut out, f, c_name, &m.path, prefix, &structs);
            }
            out.push_str("}\n\n");
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
        out.push_str("  if (err.code != 0) {\n");
        out.push_str("    napi_throw_error(env, NULL, err.message);\n");
        out.push_str("    weaveffi_error_clear(&err);\n");
        out.push_str("    return NULL;\n");
        out.push_str("  }\n");
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

/// The listener context + registry shared by every generated listener. The
/// registry is only mutated from the JS thread (register/unregister are plain
/// N-API calls), so a simple singly-linked list suffices.
fn render_listener_support_c(out: &mut String, prefix: &str) {
    out.push_str(&format!("typedef struct {prefix}_napi_listener_ctx {{\n"));
    out.push_str("    napi_threadsafe_function tsfn;\n");
    out.push_str("    uint64_t id;\n");
    out.push_str(&format!("    struct {prefix}_napi_listener_ctx* next;\n"));
    out.push_str(&format!("}} {prefix}_napi_listener_ctx;\n\n"));
    out.push_str(&format!(
        "static {prefix}_napi_listener_ctx* {prefix}_napi_listeners = NULL;\n\n"
    ));
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
    out.push_str("typedef struct {\n");
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
                out.push_str(&format!("    {} {n0};\n", slots[0].ty.render_c(prefix)));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("    char* {n0};\n"));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("    uint8_t* {n0};\n"));
                out.push_str(&format!("    size_t {};\n", slots[1].name));
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                out.push_str(&format!("    void* {n0};\n"));
            }
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str(&format!("    char* {n0};\n"));
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => {
                    out.push_str(&format!("    int {n0}_has;\n"));
                    out.push_str(&format!("    uint8_t* {n0};\n"));
                    out.push_str(&format!("    size_t {};\n", slots[1].name));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!("    void* {n0};\n"));
                }
                other => {
                    out.push_str(&format!("    int {n0}_has;\n"));
                    out.push_str(&format!(
                        "    {} {n0};\n",
                        abi::element_ctype(other, "").render_c(prefix)
                    ));
                }
            },
            TypeRef::List(inner) => {
                let elem = elem_payload_ctype(inner, prefix);
                out.push_str(&format!("    {elem}* {n0};\n"));
                out.push_str(&format!("    size_t {};\n", slots[1].name));
            }
            TypeRef::Map(k, v) => {
                let kt = elem_payload_ctype(k, prefix);
                let vt = elem_payload_ctype(v, prefix);
                out.push_str(&format!("    {kt}* {n0};\n"));
                out.push_str(&format!("    {vt}* {};\n", slots[1].name));
                out.push_str(&format!("    size_t {};\n", slots[2].name));
            }
            TypeRef::Iterator(_) => unreachable!("validated: iterator not a callback param"),
        }
    }
    out.push_str(&format!("}} {};\n\n", cb_payload_name(cb)));
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
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
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
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
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
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => out.push_str(&format!(
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
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => out.push_str(&format!(
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
        Some(TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Iterator(_)) => {
            out.push_str("    void* result;\n")
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("    char* result;\n");
                out.push_str("    int result_null;\n");
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
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
        // Ownership of struct/handle/iterator results transfers to the
        // receiver, so the pointer stays valid across the thread hop.
        Some(TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Iterator(_)) => {
            out.push_str("        ctx->result = (void*)result;\n");
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("        ctx->result_null = result == NULL;\n");
                out.push_str("        ctx->result = result ? strdup(result) : NULL;\n");
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
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
    out.push_str("        napi_value err_msg;\n");
    out.push_str(
        "        napi_create_string_utf8(env, ctx->err_msg ? ctx->err_msg : \"\", NAPI_AUTO_LENGTH, &err_msg);\n",
    );
    out.push_str("        napi_value err_obj;\n");
    out.push_str("        napi_create_error(env, NULL, err_msg, &err_obj);\n");
    out.push_str("        napi_value err_code;\n");
    out.push_str("        napi_create_int32(env, ctx->err_code, &err_code);\n");
    out.push_str("        napi_set_named_property(env, err_obj, \"code\", err_code);\n");
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
        Some(TypeRef::TypedHandle(_) | TypeRef::Iterator(_)) => {
            out.push_str("        napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n");
        }
        Some(TypeRef::Struct(name)) => {
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
            TypeRef::Struct(name) => {
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
            TypeRef::TypedHandle(_) => {
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

fn render_async_napi_body(
    out: &mut String,
    f: &FnBinding,
    c_name: &str,
    module: &str,
    prefix: &str,
) {
    let n = f.params.len();
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
    for (i, p) in f.params.iter().enumerate() {
        emit_param(
            out,
            &mut c_args,
            &mut cleanups,
            &p.ty,
            &p.name,
            i,
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
    out.push_str(&format!("  {c_name}_async({args_str});\n"));

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    out.push_str("  return promise;\n");
}

fn render_napi_body(
    out: &mut String,
    f: &FnBinding,
    c_name: &str,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
) {
    let n = f.params.len();
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
    for (i, p) in f.params.iter().enumerate() {
        emit_param(
            out,
            &mut c_args,
            &mut cleanups,
            &p.ty,
            &p.name,
            i,
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
            out.push_str(&format!("  {rt} result = {c_name}({args_str});\n"));
        }
        _ => {
            out.push_str(&format!("  {c_name}({args_str});\n"));
        }
    }

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    out.push_str("  if (err.code != 0) {\n");
    out.push_str("    napi_throw_error(env, NULL, err.message);\n");
    out.push_str("    weaveffi_error_clear(&err);\n");
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");

    match &f.ret {
        Some(ret) => emit_ret_to_napi(out, ret, module, prefix, &f.name, structs),
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
        TypeRef::Struct(s) => {
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
        TypeRef::Struct(s) => {
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
        TypeRef::Struct(_) => {
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
            let kt = c_elem_type(k, module, prefix);
            let vt = c_elem_type(v, module, prefix);
            out.push_str(&format!("  {kt}* out_keys = NULL;\n"));
            out.push_str(&format!("  {vt}* out_values = NULL;\n"));
            out.push_str("  size_t out_len = 0;\n");
            c_args.push("out_keys".into());
            c_args.push("out_values".into());
            c_args.push("&out_len".into());
        }
        TypeRef::Optional(inner) if is_c_ptr_type(inner) => {
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
/// value) into the napi value `target`. Owned element strings are freed after
/// the copy, matching the C ABI's transfer-on-return contract.
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
        TypeRef::Struct(name) => {
            emit_struct_to_object(
                out, env, name, expr, target, module, prefix, structs, indent, false,
            );
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
        TypeRef::Struct(name) => {
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
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Struct(_)) => {
            let TypeRef::Struct(name) = inner.as_ref() else {
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
        // An optional typed handle lowers to a nullable opaque pointer that the
        // field surfaces as the integer handle (or null), like the non-optional
        // case but guarded on NULL.
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::TypedHandle(_)) => {
            let TypeRef::TypedHandle(name) = inner.as_ref() else {
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
                "{indent}  else {{ void* {fv}_buf; napi_create_buffer_copy({env}, {fv}_len, {fv}_data, &{fv}_buf, &{fv}); }}\n"
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
            out.push_str(&format!("{indent}  napi_create_object({env}, &{fv});\n"));
            out.push_str(&format!(
                "{indent}  if ({fv}_keys != NULL && {fv}_vals != NULL) {{\n"
            ));
            out.push_str(&format!(
                "{indent}    for (size_t {fv}_i = 0; {fv}_i < {fv}_len; {fv}_i++) {{\n"
            ));
            out.push_str(&format!("{indent}      napi_value {fv}_v;\n"));
            emit_elem_to_napi(
                out,
                env,
                v,
                &format!("{fv}_vals[{fv}_i]"),
                &format!("{fv}_v"),
                module,
                prefix,
                structs,
                &format!("{indent}      "),
            );
            match k.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str(&format!(
                        "{indent}      napi_set_named_property({env}, {fv}, {fv}_keys[{fv}_i], {fv}_v);\n"
                    ));
                    if matches!(k.as_ref(), TypeRef::StringUtf8) {
                        out.push_str(&format!(
                            "{indent}      weaveffi_free_string((char*){fv}_keys[{fv}_i]);\n"
                        ));
                    }
                }
                other => {
                    out.push_str(&format!("{indent}      napi_value {fv}_k;\n"));
                    out.push_str(&format!(
                        "{indent}      {}\n",
                        napi_create_leaf(
                            env,
                            other,
                            &format!("{fv}_keys[{fv}_i]"),
                            &format!("{fv}_k")
                        )
                    ));
                    out.push_str(&format!(
                        "{indent}      napi_set_property({env}, {fv}, {fv}_k, {fv}_v);\n"
                    ));
                }
            }
            out.push_str(&format!("{indent}    }}\n"));
            out.push_str(&format!("{indent}  }}\n"));
            out.push_str(&format!("{indent}}}\n"));
        }
        _ => out.push_str(&format!("{indent}napi_get_null({env}, &{fv});\n")),
    }
}

fn emit_ret_to_napi(
    out: &mut String,
    ty: &TypeRef,
    module: &str,
    prefix: &str,
    fn_name: &str,
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
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
        }
        TypeRef::Struct(name) => {
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
        TypeRef::Map(_, _) => {
            out.push_str("  napi_create_object(env, &ret);\n");
        }
        TypeRef::Iterator(inner) => {
            let fn_pascal = fn_name.to_upper_camel_case();
            let iter_type = format!("{prefix}_{module}_{fn_pascal}Iterator");
            let et = c_elem_type(inner, module, prefix);
            out.push_str("  napi_create_array(env, &ret);\n");
            out.push_str("  uint32_t iter_idx = 0;\n");
            out.push_str(&format!("  {et} iter_item;\n"));
            // The iterator's `_next` reports per-step faults through a trailing
            // error out-param; it is part of the C ABI signature and must be
            // threaded through even when we surface drained items as an array.
            out.push_str("  weaveffi_error iter_err = {0};\n");
            out.push_str(&format!(
                "  while ({iter_type}_next(result, &iter_item, &iter_err)) {{\n"
            ));
            out.push_str("    napi_value elem;\n");
            match inner.as_ref() {
                TypeRef::I32 => {
                    out.push_str("    napi_create_int32(env, iter_item, &elem);\n");
                }
                TypeRef::U32 => {
                    out.push_str("    napi_create_uint32(env, iter_item, &elem);\n");
                }
                TypeRef::I64 => {
                    out.push_str("    napi_create_int64(env, iter_item, &elem);\n");
                }
                TypeRef::F64 => {
                    out.push_str("    napi_create_double(env, iter_item, &elem);\n");
                }
                TypeRef::I8 | TypeRef::I16 => {
                    out.push_str("    napi_create_int32(env, iter_item, &elem);\n");
                }
                TypeRef::U8 | TypeRef::U16 => {
                    out.push_str("    napi_create_uint32(env, iter_item, &elem);\n");
                }
                TypeRef::U64 => {
                    out.push_str("    napi_create_int64(env, (int64_t)iter_item, &elem);\n");
                }
                TypeRef::F32 => {
                    out.push_str("    napi_create_double(env, iter_item, &elem);\n");
                }
                TypeRef::Bool => {
                    out.push_str("    napi_get_boolean(env, iter_item, &elem);\n");
                }
                TypeRef::TypedHandle(_) | TypeRef::Handle => {
                    out.push_str(
                        "    napi_create_int64(env, (int64_t)(intptr_t)iter_item, &elem);\n",
                    );
                }
                TypeRef::StringUtf8 => {
                    out.push_str(
                        "    napi_create_string_utf8(env, iter_item, NAPI_AUTO_LENGTH, &elem);\n",
                    );
                    out.push_str("    weaveffi_free_string(iter_item);\n");
                }
                TypeRef::Struct(_) | TypeRef::Enum(_) => {
                    out.push_str(
                        "    napi_create_int64(env, (int64_t)(intptr_t)iter_item, &elem);\n",
                    );
                }
                _ => {
                    out.push_str("    napi_create_int64(env, (int64_t)iter_item, &elem);\n");
                }
            }
            out.push_str("    napi_set_element(env, ret, iter_idx++, elem);\n");
            out.push_str("  }\n");
            out.push_str(&format!("  {iter_type}_destroy(result);\n"));
        }
    }
    out.push_str("  return ret;\n");
}

fn emit_optional_ret_inner(
    out: &mut String,
    inner: &TypeRef,
    module: &str,
    prefix: &str,
    structs: &HashMap<String, StructBinding>,
) {
    match inner {
        TypeRef::I32 => {
            out.push_str("    napi_create_int32(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::U32 => {
            out.push_str("    napi_create_uint32(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::I64 => {
            out.push_str("    napi_create_int64(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::F64 => {
            out.push_str("    napi_create_double(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::I8 | TypeRef::I16 => {
            out.push_str("    napi_create_int32(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::U8 | TypeRef::U16 => {
            out.push_str("    napi_create_uint32(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::U64 => {
            out.push_str("    napi_create_int64(env, (int64_t)*result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::F32 => {
            out.push_str("    napi_create_double(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::Bool => {
            out.push_str("    napi_get_boolean(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str("    napi_create_int64(env, (int64_t)(intptr_t)*result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::Enum(_) => {
            out.push_str("    napi_create_int32(env, (int32_t)*result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::StringUtf8 => {
            out.push_str("    napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str("    weaveffi_free_string(result);\n");
        }
        TypeRef::Struct(name) => {
            emit_struct_to_object(
                out, "env", name, "result", "ret", module, prefix, structs, "    ", true,
            );
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
        TypeRef::Struct(name) => {
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
        _ => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)result[ret_i], &elem);\n"
        )),
    }
    out.push_str(&format!(
        "{ind}  napi_set_element(env, ret, (uint32_t)ret_i, elem);\n"
    ));
    out.push_str(&format!("{ind}}}\n"));
    out.push_str(&format!("{ind}free(result);\n"));
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
        // Structs, enums, and typed handles surface as bare local TS names. A
        // cross-module reference (e.g. `handle<Store>` resolved to `kv.Store`)
        // must annotate the *local* interface `Store`; the qualified IR name is
        // not a declared TS type in this module.
        TypeRef::TypedHandle(name) => local_type_name(name).to_string(),
        TypeRef::Struct(name) => local_type_name(name).to_string(),
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
        TypeRef::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("{t}[]")
        }
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

/// `.d.ts` for a rich (algebraic) enum: a class with a static factory per
/// variant (`Shape.circle(radius)`), a `tag()` discriminant reader, a frozen
/// `Tag` discriminant map, per-variant namespaced field getters
/// (`circleRadius`), and `destroy()`. Mirrors the JS class in [`render_node_index`].
fn render_rich_enum_dts(out: &mut String, e: &EnumBinding) {
    let Some(rich) = &e.rich else {
        return;
    };
    let name = &e.name;
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("export class {name} {{\n"));
    for v in &rich.variants {
        let factory = v.name.to_lower_camel_case();
        let params: Vec<String> = v
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, ts_type_for(&f.ty)))
            .collect();
        emit_doc(out, &v.doc, "  ");
        out.push_str(&format!(
            "  static {factory}({}): {name};\n",
            params.join(", ")
        ));
    }
    out.push_str("  /** The active variant's discriminant. */\n");
    out.push_str("  tag(): number;\n");
    for v in &rich.variants {
        for f in &v.fields {
            let getter = format!(
                "{}{}",
                v.name.to_lower_camel_case(),
                f.name.to_upper_camel_case()
            );
            emit_doc(out, &f.doc, "  ");
            out.push_str(&format!("  get {getter}(): {};\n", ts_type_for(&f.ty)));
        }
    }
    out.push_str("  /** Free the underlying native object. */\n");
    out.push_str("  destroy(): void;\n");
    out.push_str("}\n");
    // The discriminant map, e.g. `Shape.Tag.Circle === 1`.
    out.push_str(&format!("export namespace {name} {{\n"));
    out.push_str("  const Tag: Readonly<{\n");
    for v in &e.variants {
        out.push_str(&format!("    {}: {},\n", v.name, v.value));
    }
    out.push_str("  }>;\n");
    out.push_str("}\n");
}

fn render_struct_builder_dts(out: &mut String, s: &StructBinding) {
    let name = &s.name;
    emit_doc(out, &s.doc, "");
    out.push_str(&format!("export interface {}Builder {{\n", s.name));
    for field in &s.fields {
        let method = format!("with{}", field.name.to_upper_camel_case());
        let ts = ts_type_for(&field.ty);
        emit_doc(out, &field.doc, "  ");
        out.push_str(&format!("  {method}(value: {ts}): {name}Builder;\n"));
    }
    out.push_str(&format!("  build(): {name};\n"));
    out.push_str("}\n");
}

/// The set of *local* names of every rich (algebraic) enum in the model. Used
/// to recognize a rich enum where it surfaces as `TypeRef::Struct` in a
/// function signature (rich enums lower to opaque struct pointers).
fn rich_enum_names(model: &BindingModel) -> HashSet<String> {
    model
        .modules
        .iter()
        .flat_map(|m| m.enums.iter())
        .filter(|e| e.is_rich())
        .map(|e| e.name.clone())
        .collect()
}

/// If `ty` is a rich enum carried directly (or as an `Optional`), return its
/// local class name plus whether it was optional. Deeper nestings (list/map)
/// return `None`: those flow through the raw addon binding unwrapped.
fn rich_struct_ref(ty: &TypeRef, rich: &HashSet<String>) -> Option<(String, bool)> {
    match ty {
        TypeRef::Struct(n) if rich.contains(local_type_name(n)) => {
            Some((local_type_name(n).to_string(), false))
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(n) if rich.contains(local_type_name(n)) => {
                Some((local_type_name(n).to_string(), true))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The JS loader (`index.js`). Without rich enums it simply re-exports the
/// native addon (the historical behavior). With rich enums it layers idiomatic
/// wrapper classes, opaque-handle objects with per-variant factories, a `tag()`
/// reader, namespaced field getters, and `destroy()` (plus a
/// `FinalizationRegistry` safety net), and rewraps the handful of module
/// functions that take or return a rich enum so they speak the class, not the
/// raw handle.
fn render_node_index(api: &Api, prefix: &str, strip: bool, input_basename: &str) -> String {
    let model = BindingModel::build(api, prefix);
    let dbl = CommentStyle::DoubleSlash;
    let mut out = render_prelude(dbl, input_basename);
    out.push_str(
        "// Prefer the default node-gyp output path; fall back to a\n\
         // prebuilt index.node placed next to this file.\n\
         let addon;\n\
         try {\n  addon = require('./build/Release/weaveffi.node');\n} catch (e) {\n  addon = require('./index.node');\n}\n",
    );

    let rich = rich_enum_names(&model);
    if rich.is_empty() {
        out.push_str("module.exports = addon;\n\n");
        out.push_str(&render_trailer(dbl, "index.js"));
        return out;
    }

    // The native bindings are defined as non-enumerable properties, so copy
    // them by explicit own-name lookup before layering the idiomatic wrappers.
    out.push_str(
        "\n// Re-export every native binding, then layer idiomatic wrappers for\n\
         // rich (algebraic) enums on top.\n\
         const wv = {};\n\
         for (const _name of Object.getOwnPropertyNames(addon)) {\n  wv[_name] = addon[_name];\n}\n\n",
    );

    for m in &model.modules {
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum_class_js(&mut out, e, &m.path, strip);
            }
        }
    }

    // Rewrap module functions whose parameters or return carry a rich enum so
    // callers pass and receive the class instead of the raw opaque handle.
    for m in &model.modules {
        for f in &m.functions {
            if f.is_async {
                continue;
            }
            let ret_rich = f.ret.as_ref().and_then(|r| rich_struct_ref(r, &rich));
            let param_rich: Vec<Option<(String, bool)>> = f
                .params
                .iter()
                .map(|p| rich_struct_ref(&p.ty, &rich))
                .collect();
            if ret_rich.is_none() && param_rich.iter().all(Option::is_none) {
                continue;
            }
            let js = wrapper_name(&m.path, &f.name, strip);
            let param_names: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
            let call_args: Vec<String> = f
                .params
                .iter()
                .zip(&param_rich)
                .map(|(p, r)| match r {
                    Some((en, _)) => {
                        format!("{n} instanceof {en} ? {n}._handle : {n}", n = p.name)
                    }
                    None => p.name.clone(),
                })
                .collect();
            let inner = format!("addon.{js}({})", call_args.join(", "));
            out.push_str(&format!(
                "wv.{js} = function ({}) {{\n",
                param_names.join(", ")
            ));
            match ret_rich {
                Some((en, false)) => {
                    out.push_str(&format!("  return new {en}({inner});\n"));
                }
                Some((en, true)) => {
                    out.push_str(&format!("  const _r = {inner};\n"));
                    out.push_str(&format!("  return _r == null ? null : new {en}(_r);\n"));
                }
                None => {
                    out.push_str(&format!("  return {inner};\n"));
                }
            }
            out.push_str("};\n");
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

    out.push_str(&format!("class {name} {{\n"));
    out.push_str("  constructor(handle) {\n");
    out.push_str("    this._handle = handle;\n");
    out.push_str(&format!(
        "    {name}._cleanup.register(this, handle, this);\n"
    ));
    out.push_str("  }\n");

    // Per-variant factories (`Shape.circle(radius)`).
    for v in &rich.variants {
        let factory = v.name.to_lower_camel_case();
        let ctor_js = wrapper_name(module, &rich_ctor_base(name, &v.name), strip);
        let params: Vec<String> = v.fields.iter().map(|f| f.name.clone()).collect();
        let joined = params.join(", ");
        out.push_str(&format!(
            "  static {factory}({joined}) {{\n    return new {name}(addon.{ctor_js}({joined}));\n  }}\n"
        ));
    }

    // Discriminant reader.
    let tag_js = wrapper_name(module, &rich_tag_base(name), strip);
    out.push_str(&format!(
        "  tag() {{\n    return addon.{tag_js}(this._handle);\n  }}\n"
    ));

    // Namespaced per-variant field getters (`circleRadius`).
    for v in &rich.variants {
        for f in &v.fields {
            let getter = format!(
                "{}{}",
                v.name.to_lower_camel_case(),
                f.name.to_upper_camel_case()
            );
            let getter_js = wrapper_name(module, &rich_getter_base(name, &v.name, &f.name), strip);
            out.push_str(&format!(
                "  get {getter}() {{\n    return addon.{getter_js}(this._handle);\n  }}\n"
            ));
        }
    }

    // Explicit cleanup; guarded so a double `destroy()` (or destroy-then-GC) is
    // a no-op rather than a double free.
    out.push_str("  destroy() {\n");
    out.push_str("    if (this._handle) {\n");
    out.push_str(&format!("      {name}._cleanup.unregister(this);\n"));
    out.push_str(&format!("      addon.{destroy_js}(this._handle);\n"));
    out.push_str("      this._handle = 0;\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("}\n");

    out.push_str(&format!(
        "{name}._cleanup = new FinalizationRegistry((handle) => {{\n  if (handle) {{ addon.{destroy_js}(handle); }}\n}});\n"
    ));

    // Frozen discriminant map (`Shape.Tag.Circle === 1`).
    let consts: Vec<String> = e
        .variants
        .iter()
        .map(|v| format!("{}: {}", v.name, v.value))
        .collect();
    out.push_str(&format!(
        "{name}.Tag = Object.freeze({{ {} }});\n",
        consts.join(", ")
    ));
    out.push_str(&format!("wv.{name} = {name};\n\n"));
}

fn render_node_dts(
    api: &Api,
    prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let model = BindingModel::build(api, prefix);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str("// Generated types for WeaveFFI functions\n");
    for m in &model.modules {
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
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                continue;
            };
            let cb_params: Vec<String> = cb
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
                .collect();
            let register = wrapper_name(
                &m.path,
                &format!("register_{}", l.name),
                strip_module_prefix,
            );
            let unregister = wrapper_name(
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
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
                .collect();
            let base_ret = match &f.ret {
                Some(ty) => ts_type_for(ty),
                None => "void".into(),
            };
            let ret = if f.is_async {
                format!("Promise<{base_ret}>")
            } else {
                base_ret
            };
            let ts_name = wrapper_name(&m.path, &f.name, strip_module_prefix);
            let mut tags = vec![format!("Maps to C function: {}", f.c_base)];
            if let Some(msg) = &f.deprecated {
                tags.push(format!("@deprecated {}", msg));
            }
            emit_fn_doc(&mut out, &f.doc, &f.params, "", &tags);
            out.push_str(&format!(
                "export function {}({}): {}\n",
                ts_name,
                params.join(", "),
                ret
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
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

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
            .find(|f| f.path.as_str().ends_with("npm/weaveffi-win32-x64/package.json"))
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
            version: "0.4.0".into(),
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
            modules: vec![],
        }
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
                "export function events_register_message_listener(callback: (message: string) => void): number"
            ),
            "register dts missing: {dts}"
        );
        assert!(
            dts.contains("export function events_unregister_message_listener(id: number): void"),
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
        assert_eq!(ts_type_for(&TypeRef::Struct("Contact".into())), "Contact");
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
        assert_eq!(ts_type_for(&TypeRef::Struct("kv.Store".into())), "Store");
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
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into(),
            )))),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        m.functions.push(Function {
            name: "list_contacts".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });

        let dts = render_node_dts(&make_api(vec![m]), "weaveffi", true, "weaveffi.yml");

        assert!(dts.contains("export interface Contact {"));
        assert!(dts.contains("  name: string;"));
        assert!(dts.contains("  age: number;"));
        assert!(dts.contains("  active: boolean;"));
        assert!(dts.contains("export enum Color {"));
        assert!(dts.contains("  Red = 0,"));
        assert!(dts.contains("  Green = 1,"));
        assert!(dts.contains("  Blue = 2,"));
        assert!(dts.contains("export function get_contact(id: number): Contact | null"));
        assert!(dts.contains("export function list_contacts(): Contact[]"));

        let iface_pos = dts.find("export interface Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function get_contact").unwrap();
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
                    name: "list_contacts".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
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
            dts.contains("export function get_contact(id: number): Contact | null"),
            "missing get_contact with optional return: {dts}"
        );
        assert!(
            dts.contains("export function list_contacts(): Contact[]"),
            "missing list_contacts with list return: {dts}"
        );
        assert!(
            dts.contains(
                "export function set_favorite_color(contact_id: number, color: Color | null): void"
            ),
            "missing set_favorite_color with optional enum param: {dts}"
        );
        assert!(
            dts.contains("export function get_tags(contact_id: number): string[]"),
            "missing get_tags with list return: {dts}"
        );

        let iface_pos = dts.find("export interface Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function get_contact").unwrap();
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
                deprecated: None,
                since: None,
            });
            m
        }]);

        let dts = render_node_dts(&api, "weaveffi", true, "weaveffi.yml");

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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
            dts.contains("export function create_contact("),
            "stripped name should be create_contact: {dts}"
        );
        assert!(
            !dts.contains("export function contacts_create_contact("),
            "should not contain module-prefixed name: {dts}"
        );

        let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("\"create_contact\""),
            "JS export name should be stripped: {addon}"
        );
        assert!(
            addon.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {addon}"
        );

        let no_strip = NodeConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_node_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        NodeGenerator.generate(&api, out_dir2, &no_strip).unwrap();

        let dts2 = std::fs::read_to_string(tmp2.join("node/types.d.ts")).unwrap();
        assert!(
            dts2.contains("export function contacts_create_contact("),
            "default should use module-prefixed name: {dts2}"
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let dts = render_node_dts(&api, "weaveffi", true, "weaveffi.yml");
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
        let dts = render_node_dts(&api, "weaveffi", true, "weaveffi.yml");
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
        let dts = render_node_dts(&api, "weaveffi", true, "weaveffi.yml");
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
        let dts = render_node_dts(&api, "weaveffi", true, "weaveffi.yml");
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
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let dts = render_node_dts(&api, "weaveffi", true, "weaveffi.yml");
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, "weaveffi", true, "weaveffi.yml");
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
    fn node_emits_doc_on_function() {
        let dts = render_node_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            true,
            "weaveffi.yml",
        );
        assert!(dts.contains("Performs a thing."), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_struct() {
        let dts = render_node_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            true,
            "weaveffi.yml",
        );
        assert!(dts.contains("/** An item we track. */"), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_enum_variant() {
        let dts = render_node_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            true,
            "weaveffi.yml",
        );
        assert!(dts.contains("/** Kind of item. */"), "{dts}");
        assert!(dts.contains("/** A small one */"), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_field() {
        let dts = render_node_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            true,
            "weaveffi.yml",
        );
        assert!(dts.contains("/** Stable id */"), "{dts}");
    }

    #[test]
    fn node_emits_doc_on_param() {
        let dts = render_node_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            true,
            "weaveffi.yml",
        );
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
            modules: vec![],
        }
    }

    #[test]
    fn rich_enum_addon_exposes_native_helpers() {
        let addon = render_addon_c(
            &make_api(vec![shapes_module()]),
            "weaveffi",
            false,
            "shapes.yml",
        );

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
        let addon = render_addon_c(
            &make_api(vec![shapes_module()]),
            "weaveffi",
            false,
            "shapes.yml",
        );

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
        let index = render_node_index(
            &make_api(vec![shapes_module()]),
            "weaveffi",
            false,
            "shapes.yml",
        );

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
        // The factories call the native constructors.
        assert!(
            index.contains("return new Shape(addon.shapes_Shape_circle_new(radius));"),
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
            index.contains("wv.shapes_scale = function (shape, factor) {")
                && index.contains("return new Shape(addon.shapes_scale(shape instanceof Shape ? shape._handle : shape, factor));"),
            "scale must be rewrapped to return a Shape: {index}"
        );
        assert!(
            index.contains(
                "return addon.shapes_describe(shape instanceof Shape ? shape._handle : shape);"
            ),
            "describe must unwrap a Shape argument: {index}"
        );
        // A function with no rich enum is left as the raw native binding.
        assert!(
            !index.contains("wv.shapes_sum_bytes = function"),
            "sum_bytes must not be rewrapped: {index}"
        );
    }

    #[test]
    fn rich_enum_index_js_without_rich_is_plain_reexport() {
        // A model with no rich enums keeps the historical `module.exports = addon`.
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
            deprecated: None,
            since: None,
        });
        let index = render_node_index(&make_api(vec![m]), "weaveffi", false, "weaveffi.yml");
        assert!(
            index.contains("module.exports = addon;"),
            "no-rich-enum index must re-export the addon directly: {index}"
        );
        assert!(
            !index.contains("class "),
            "no class should be emitted: {index}"
        );
    }

    #[test]
    fn rich_enum_dts_emits_class_not_enum() {
        let dts = render_node_dts(
            &make_api(vec![shapes_module()]),
            "weaveffi",
            false,
            "shapes.yml",
        );

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

        // Free functions are typed in terms of the class.
        assert!(
            dts.contains("export function shapes_describe(shape: Shape): string"),
            "{dts}"
        );
        assert!(
            dts.contains("export function shapes_scale(shape: Shape, factor: number): Shape"),
            "{dts}"
        );
    }
}
