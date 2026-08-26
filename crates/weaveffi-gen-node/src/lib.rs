//! Node.js (N-API) binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader plus TypeScript type definitions for the
//! companion N-API addon. Records and rich enums are value types: they cross
//! the ABI serialized in WeaveFFI value buffers, so records surface as plain
//! JS objects, rich enums as tagged unions, and the loader carries a small
//! private buffer writer/reader plus one pack and one unpack function per
//! type. Async functions surface as `Promise`-returning functions, `iter<T>`
//! functions surface as lazy `IterableIterator<T>` wrappers that pull one
//! element per step, interfaces surface as JS classes over opaque native
//! handles, and each declared error domain surfaces as an `Error` subclass
//! extending the generic `WeaveFFIError` brand. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::ToLowerCamelCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, is_buffered};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::{type_name as error_type_name, ERROR_BRAND};
use weaveffi_core::model::{
    iterator_item_ctype, BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding,
    FnBinding, InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::{elem_free, ElemFree};
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
/// generated classes, not public API, so they keep the raw member spelling.
fn iface_member_base(iface: &str, member: &str) -> String {
    format!("{iface}_{member}")
}

/// Escape a string for embedding in a single-quoted JS literal.
fn js_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
}

/// The C return-type spelling of `ty` at a call site. Buffered values render
/// as `const uint8_t*` (the encoded buffer); an iterator launcher's handle is
/// held as `void*` so the shared state cell can adopt it.
fn c_ret_type_str(ty: &TypeRef, module: &str, prefix: &str) -> String {
    if matches!(ty, TypeRef::Iterator(_)) {
        return "void*".into();
    }
    abi::lower_return(ty, module).ret.render_c(prefix)
}

/// The bare C type of a scalar (or C-enum-free leaf) parameter temporary.
fn c_scalar_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "int8_t",
        TypeRef::I16 => "int16_t",
        TypeRef::I32 => "int32_t",
        TypeRef::I64 => "int64_t",
        TypeRef::U8 => "uint8_t",
        TypeRef::U16 => "uint16_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::U64 => "uint64_t",
        TypeRef::F32 => "float",
        TypeRef::F64 => "double",
        TypeRef::Bool => "bool",
        _ => unreachable!("not a scalar type"),
    }
}

fn napi_getter(ty: &TypeRef) -> &'static str {
    match ty {
        // i8/i16 are read through the 32-bit signed getter (N-API has no
        // narrower int getter) and narrowed at the use site.
        TypeRef::I8 | TypeRef::I16 | TypeRef::I32 | TypeRef::Enum(_) => "napi_get_value_int32",
        TypeRef::U8 | TypeRef::U16 | TypeRef::U32 => "napi_get_value_uint32",
        // u64 mirrors i64/handle: read as a 64-bit int, reinterpreted as needed.
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => {
            "napi_get_value_int64"
        }
        // f32 is read as a double then narrowed to float at the use site.
        TypeRef::F32 | TypeRef::F64 => "napi_get_value_double",
        TypeRef::Bool => "napi_get_value_bool",
        _ => "napi_get_value_int64",
    }
}

/// The C type of the temporary an N-API getter writes into for a scalar that is
/// narrower than the getter's natural width. N-API only exposes 32/64-bit int
/// and `double` getters, so `i8/i16/u8/u16/f32` must be read into a wider
/// temporary and then narrowed with an explicit cast to the real ABI type;
/// `u64` is read as `int64_t` then reinterpreted.
fn napi_read_tmp_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 | TypeRef::I16 => "int32_t",
        TypeRef::U8 | TypeRef::U16 => "uint32_t",
        TypeRef::U64 => "int64_t",
        TypeRef::F32 => "double",
        _ => "int64_t",
    }
}

/// Emit `{prefix}_napi_error_value`, the shared constructor of the JS error
/// object every failure path produces: a plain `Error` carrying the numeric
/// ABI code as a `code` property and, when the producer attached one, the
/// structured payload buffer as a `payload` property. The JS loader rebrands
/// it as the generic `WeaveFFIError` or the module's typed domain class and
/// decodes the payload fields there.
fn render_error_value_helper_c(out: &mut String, prefix: &str) {
    out.push_str(&format!(
        "static napi_value {prefix}_napi_error_value(napi_env env, int32_t code, const char* message, const uint8_t* payload_ptr, size_t payload_len) {{\n"
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
    out.push_str("    if (payload_ptr != NULL) {\n");
    out.push_str("        napi_value payload_val;\n");
    out.push_str(
        "        napi_create_buffer_copy(env, payload_len, payload_ptr, NULL, &payload_val);\n",
    );
    out.push_str("        napi_set_named_property(env, err, \"payload\", payload_val);\n");
    out.push_str("    }\n");
    out.push_str("    return err;\n");
    out.push_str("}\n\n");
}

/// Emit the post-call `out_err` check: throw the code-carrying JS error (with
/// the borrowed payload buffer copied in) and bail on a non-zero slot, then
/// clear the error, which releases both the message and the payload. The JS
/// loader maps the `code` property to the module's typed domain class
/// (throwing callables) or the generic brand.
fn emit_error_check_c(out: &mut String, prefix: &str) {
    out.push_str("  if (err.code != 0) {\n");
    out.push_str(&format!(
        "    napi_throw(env, {prefix}_napi_error_value(env, err.code, err.message, err.payload_ptr, err.payload_len));\n"
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
/// `weaveffi_free_string` after the JS string is created, and byte or
/// buffered elements are copied into a JS `Buffer` and released with
/// `weaveffi_free_bytes` (the JS wrapper decodes buffered elements).
fn render_iterator_napi_fns(
    out: &mut String,
    f: &FnBinding,
    ib: &IteratorBinding,
    module: &str,
    prefix: &str,
) {
    let c_name = &f.c_base;
    let tag = &ib.iter_tag;
    let next_sym = &ib.next.symbol;
    let destroy_sym = &ib.destroy_symbol;
    let ef = elem_free(&ib.elem);

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
    let et = iterator_item_ctype(&ib.elem, module).render_c(prefix);
    out.push_str(&format!("  {et} iter_item;\n"));
    if ef == ElemFree::Bytes {
        out.push_str("  size_t iter_item_len = 0;\n");
    }
    out.push_str("  weaveffi_error iter_err = {0};\n");
    let next_args = if ef == ElemFree::Bytes {
        format!("({tag}*)state->iter, &iter_item, &iter_item_len, &iter_err")
    } else {
        format!("({tag}*)state->iter, &iter_item, &iter_err")
    };
    out.push_str(&format!("  if (!{next_sym}({next_args})) {{\n"));
    out.push_str(&format!("    {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("    state->iter = NULL;\n");
    out.push_str("    if (iter_err.code != 0) {\n");
    out.push_str(&format!(
        "      napi_throw(env, {prefix}_napi_error_value(env, iter_err.code, iter_err.message, iter_err.payload_ptr, iter_err.payload_len));\n"
    ));
    out.push_str("      weaveffi_error_clear(&iter_err);\n");
    out.push_str("      return NULL;\n");
    out.push_str("    }\n");
    out.push_str("    napi_get_undefined(env, &ret);\n");
    out.push_str("    return ret;\n");
    out.push_str("  }\n");
    match ef {
        ElemFree::String => {
            out.push_str(
                "  napi_create_string_utf8(env, iter_item ? iter_item : \"\", NAPI_AUTO_LENGTH, &ret);\n",
            );
            out.push_str("  weaveffi_free_string((char*)iter_item);\n");
        }
        ElemFree::Bytes => {
            out.push_str("  napi_create_buffer_copy(env, iter_item_len, iter_item, NULL, &ret);\n");
            out.push_str("  weaveffi_free_bytes((uint8_t*)iter_item, iter_item_len);\n");
        }
        ElemFree::None => {
            out.push_str(&format!(
                "  {}\n",
                napi_create_leaf("env", &ib.elem, "iter_item", "ret")
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
fn render_callable_napi(
    out: &mut String,
    all_exports: &mut Vec<(String, String)>,
    f: &FnBinding,
    js_name: String,
    module: &str,
    prefix: &str,
    self_tag: Option<&str>,
) {
    let c_name = &f.c_base;
    let napi_name = format!("Napi_{c_name}");

    if f.is_async {
        render_async_machinery(out, f, c_name, module, prefix);
    }
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator_napi_fns(out, f, ib, module, prefix);
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
        render_napi_body(out, f, module, prefix, self_tag);
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

    // Every error path (sync throws, iterator faults, async rejections)
    // funnels through one code-and-payload-carrying error constructor.
    let has_error_paths = model
        .modules
        .iter()
        .any(|m| !m.functions.is_empty() || !m.interfaces.is_empty());
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
        // Records and rich enums are value types crossing the ABI serialized
        // in value buffers, so they need no native helpers here; the JS
        // loader packs and unpacks them. Interfaces get one native entry
        // point per member (constructors and statics marshal like free
        // functions; methods additionally read the wrapped pointer from the
        // leading argument) plus the destructor the JS class's disposal path
        // calls.
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
            render_cb_calljs(&mut out, cb);
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

/// Read `args[0]` as the opaque handle and bind it to a typed `self` pointer.
/// Used by the interface destructor entry point.
fn emit_self_handle_read(out: &mut String, c_tag: &str) {
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  int64_t self_raw;\n");
    out.push_str("  napi_get_value_int64(env, args[0], &self_raw);\n");
    out.push_str(&format!(
        "  {c_tag}* self = ({c_tag}*)(intptr_t)self_raw;\n"
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
    emit_self_handle_read(out, &i.c_tag);
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
/// trampoline, freed in the call-js marshaller). Buffered arguments arrive as
/// borrowed `ptr` + `len` pairs valid only for the dispatch, so their bytes
/// are copied exactly like a `bytes` argument; the JS loader decodes the
/// copied buffer before invoking the user callback.
fn render_cb_payload_struct(out: &mut String, cb: &CallbackBinding, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.block(
        "typedef struct {",
        format!("}} {};", cb_payload_name(cb)),
        |w| {
            for p in &cb.params {
                let slots = abi::lower_param(&p.name, &p.ty, "", false);
                let n0 = &slots[0].name;
                if is_buffered(&p.ty) || matches!(p.ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
                    w.line(format!("uint8_t* {n0};"));
                    w.line(format!("size_t {};", slots[1].name));
                    continue;
                }
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
                    TypeRef::TypedHandle(_) => {
                        w.line(format!("void* {n0};"));
                    }
                    // Only `Interface?` reaches here (every other optional is
                    // buffered): a nullable borrowed object pointer.
                    TypeRef::Optional(_) => {
                        w.line(format!("void* {n0};"));
                    }
                    TypeRef::Record(_)
                    | TypeRef::RichEnum(_)
                    | TypeRef::List(_)
                    | TypeRef::Map(_, _)
                    | TypeRef::Bytes
                    | TypeRef::BorrowedBytes => {
                        unreachable!("buffered/bytes callback param handled above")
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
        if is_buffered(&p.ty) || matches!(p.ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
            let n1 = &slots[1].name;
            out.push_str(&format!("    p->{n1} = {n1};\n"));
            out.push_str(&format!(
                "    if ({n0} != NULL && {n1} > 0) {{ p->{n0} = (uint8_t*)malloc({n1}); memcpy(p->{n0}, {n0}, {n1}); }}\n"
            ));
            continue;
        }
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
            TypeRef::TypedHandle(_) | TypeRef::Optional(_) => {
                out.push_str(&format!("    p->{n0} = (void*){n0};\n"));
            }
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes => unreachable!("buffered/bytes handled above"),
            TypeRef::Iterator(_) => unreachable!("validated: iterator not a callback param"),
            TypeRef::Interface(_) => unreachable!("validated: interface not a callback param"),
            TypeRef::Named(_) => unreachable!("unresolved type reference"),
        }
    }
    out.push_str("    napi_call_threadsafe_function(ctx->tsfn, p, napi_tsfn_nonblocking);\n");
    out.push_str("}\n\n");
}

/// One payload field rendered to a `napi_value` in `argv[idx]` (call-js side).
fn emit_payload_to_napi(out: &mut String, p: &ParamBinding, idx: usize) {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = &slots[0].name;
    let target = format!("argv[{idx}]");
    if is_buffered(&p.ty) || matches!(p.ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
        let n1 = &slots[1].name;
        out.push_str(&format!(
            "        napi_create_buffer_copy(env, p->{n1}, p->{n0} ? (const void*)p->{n0} : (const void*)\"\", NULL, &{target});\n"
        ));
        return;
    }
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => out.push_str(&format!(
            "        napi_create_string_utf8(env, p->{n0} ? p->{n0} : \"\", NAPI_AUTO_LENGTH, &{target});\n"
        )),
        TypeRef::TypedHandle(_) => out.push_str(&format!(
            "        napi_create_int64(env, (int64_t)(intptr_t)p->{n0}, &{target});\n"
        )),
        // Only `Interface?` reaches here: nullable object pointer.
        TypeRef::Optional(_) => out.push_str(&format!(
            "        if (p->{n0}) napi_create_int64(env, (int64_t)(intptr_t)p->{n0}, &{target}); else napi_get_null(env, &{target});\n"
        )),
        other => {
            let leaf = payload_leaf_to_napi(other, &format!("p->{n0}"), &target);
            out.push_str(&format!("        {leaf}\n"));
        }
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

/// Frees one payload field after the JS call.
fn emit_payload_free(out: &mut String, p: &ParamBinding) {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = &slots[0].name;
    if is_buffered(&p.ty)
        || matches!(
            p.ty,
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::StringUtf8 | TypeRef::BorrowedStr
        )
    {
        out.push_str(&format!("    free(p->{n0});\n"));
    }
}

/// The JS-thread marshaller invoked by the threadsafe function: converts the
/// payload into JS arguments, calls the user callback, and frees the payload.
fn render_cb_calljs(out: &mut String, cb: &CallbackBinding) {
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
            emit_payload_to_napi(out, p, i);
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

/// The classified shape of an async function's result, driving what the
/// completion callback copies and what the settle marshaller creates.
enum AsyncResultShape {
    /// No result: the promise resolves `undefined`.
    None,
    /// A by-value scalar, bool, C-style enum, or bare handle.
    Value,
    /// An owned `const char*` string (nullable).
    Str,
    /// A `ptr` + `len` pair: an owned `bytes` result (slot named `result`).
    Bytes,
    /// A borrowed value-buffer pair (slots `result_ptr` + `result_len`); the
    /// callback must copy it before returning, and the JS wrapper decodes it.
    Buffered,
    /// An owned object pointer the callback adopts (interface, typed handle,
    /// iterator, or nullable interface).
    Object,
}

/// Classify an async result type into its marshalling shape.
fn async_result_shape(ret: Option<&TypeRef>) -> AsyncResultShape {
    let Some(ty) = ret else {
        return AsyncResultShape::None;
    };
    if is_buffered(ty) {
        return AsyncResultShape::Buffered;
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => AsyncResultShape::Str,
        TypeRef::Bytes | TypeRef::BorrowedBytes => AsyncResultShape::Bytes,
        TypeRef::TypedHandle(_)
        | TypeRef::Interface(_)
        | TypeRef::Iterator(_)
        | TypeRef::Optional(_) => AsyncResultShape::Object,
        _ => AsyncResultShape::Value,
    }
}

/// The `, <c-type> <name>` suffix of an async completion callback's result
/// slots, rendered from the shared ABI lowering so the signature matches the
/// producer's typedef exactly.
fn async_cb_result_params_node(ret: Option<&TypeRef>, module: &str, prefix: &str) -> String {
    match ret {
        None => String::new(),
        Some(ty) => abi::callback_result_params(ty, module)
            .iter()
            .map(|p| format!(", {} {}", p.ty.render_c(prefix), p.name))
            .collect(),
    }
}

/// Emit the per-async-function machinery: a context struct carrying the
/// promise + threadsafe function + deep-copied results, the producer-thread
/// completion callback (which only copies and queues), and the JS-thread
/// marshaller (which settles the promise).
///
/// The completion callback may fire on any thread, so it must never touch
/// `napi_env`; the ref'd threadsafe function also keeps the event loop alive
/// until the promise settles. Borrowed results (strings, bytes, and buffered
/// values) are deep-copied inside the callback because the producer frees
/// them after it returns; owned object results are adopted. The error's
/// message and payload are copied for the same reason.
fn render_async_machinery(
    out: &mut String,
    f: &FnBinding,
    c_name: &str,
    module: &str,
    prefix: &str,
) {
    let actx = format!("{c_name}_napi_actx");
    let cb_name = format!("{c_name}_napi_cb");
    let calljs = format!("{c_name}_napi_settle");
    let cb_result = async_cb_result_params_node(f.ret.as_ref(), module, prefix);
    let shape = async_result_shape(f.ret.as_ref());

    // -- context struct --
    out.push_str("typedef struct {\n");
    out.push_str("    napi_deferred deferred;\n");
    out.push_str("    napi_threadsafe_function tsfn;\n");
    out.push_str("    int32_t err_code;\n");
    out.push_str("    char* err_msg;\n");
    out.push_str("    uint8_t* err_payload;\n");
    out.push_str("    size_t err_payload_len;\n");
    match &shape {
        AsyncResultShape::None => {}
        AsyncResultShape::Value => {
            let ct = c_ret_type_str(
                f.ret.as_ref().expect("value shape has a type"),
                module,
                prefix,
            );
            out.push_str(&format!("    {ct} result;\n"));
        }
        AsyncResultShape::Str => {
            out.push_str("    char* result;\n");
            out.push_str("    int result_null;\n");
        }
        AsyncResultShape::Bytes | AsyncResultShape::Buffered => {
            out.push_str("    uint8_t* result;\n");
            out.push_str("    size_t result_len;\n");
        }
        AsyncResultShape::Object => {
            out.push_str("    void* result;\n");
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
    out.push_str("        ctx->err_payload_len = err->payload_len;\n");
    out.push_str(
        "        if (err->payload_ptr != NULL && err->payload_len > 0) { ctx->err_payload = (uint8_t*)malloc(err->payload_len); memcpy(ctx->err_payload, err->payload_ptr, err->payload_len); }\n",
    );
    out.push_str("    } else {\n");
    match &shape {
        AsyncResultShape::None => {}
        AsyncResultShape::Value => {
            out.push_str("        ctx->result = result;\n");
        }
        AsyncResultShape::Str => {
            out.push_str("        ctx->result_null = result == NULL;\n");
            out.push_str("        ctx->result = result ? strdup(result) : NULL;\n");
        }
        AsyncResultShape::Bytes => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result, result_len); }\n",
            );
        }
        // The buffer is borrowed for the callback's duration, so the bytes
        // are copied here; the JS wrapper decodes them after the promise
        // resolves.
        AsyncResultShape::Buffered => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result_ptr != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result_ptr, result_len); }\n",
            );
        }
        // Owned-object results (interfaces, typed handles, iterators) are
        // adopted by the receiver, so the pointer stays valid across the
        // thread hop.
        AsyncResultShape::Object => {
            out.push_str("        ctx->result = (void*)result;\n");
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
    out.push_str(&format!(
        "        napi_value err_obj = {prefix}_napi_error_value(env, ctx->err_code, ctx->err_msg, ctx->err_payload, ctx->err_payload_len);\n"
    ));
    out.push_str("        napi_reject_deferred(env, ctx->deferred, err_obj);\n");
    out.push_str("    } else {\n");
    out.push_str("        napi_value val;\n");
    match &shape {
        AsyncResultShape::None => out.push_str("        napi_get_undefined(env, &val);\n"),
        AsyncResultShape::Value => match f.ret.as_ref() {
            Some(TypeRef::I32) => {
                out.push_str("        napi_create_int32(env, ctx->result, &val);\n")
            }
            Some(TypeRef::U32) => {
                out.push_str("        napi_create_uint32(env, ctx->result, &val);\n")
            }
            Some(TypeRef::I64) => {
                out.push_str("        napi_create_int64(env, ctx->result, &val);\n")
            }
            Some(TypeRef::F64) => {
                out.push_str("        napi_create_double(env, ctx->result, &val);\n")
            }
            Some(TypeRef::I8 | TypeRef::I16) => {
                out.push_str("        napi_create_int32(env, ctx->result, &val);\n");
            }
            Some(TypeRef::U8 | TypeRef::U16) => {
                out.push_str("        napi_create_uint32(env, ctx->result, &val);\n");
            }
            Some(TypeRef::U64 | TypeRef::Handle) => {
                out.push_str("        napi_create_int64(env, (int64_t)ctx->result, &val);\n");
            }
            Some(TypeRef::F32) => {
                out.push_str("        napi_create_double(env, ctx->result, &val);\n")
            }
            Some(TypeRef::Bool) => {
                out.push_str("        napi_get_boolean(env, ctx->result, &val);\n")
            }
            Some(TypeRef::Enum(_)) => {
                out.push_str("        napi_create_int32(env, (int32_t)ctx->result, &val);\n");
            }
            _ => unreachable!("value shape covers scalars, bools, enums, and handles"),
        },
        AsyncResultShape::Str => {
            out.push_str(
                "        if (ctx->result_null) napi_get_null(env, &val); else napi_create_string_utf8(env, ctx->result ? ctx->result : \"\", NAPI_AUTO_LENGTH, &val);\n",
            );
        }
        AsyncResultShape::Bytes | AsyncResultShape::Buffered => {
            out.push_str(
                "        napi_create_buffer_copy(env, ctx->result_len, ctx->result ? (const void*)ctx->result : (const void*)\"\", NULL, &val);\n",
            );
        }
        AsyncResultShape::Object => {
            // A nullable interface result resolves `null` for the absent
            // case; every other object pointer is surfaced as the raw
            // handle the JS class adopts.
            if matches!(f.ret.as_ref(), Some(TypeRef::Optional(_))) {
                out.push_str(
                    "        if (ctx->result == NULL) napi_get_null(env, &val); else napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n",
                );
            } else {
                out.push_str(
                    "        napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n",
                );
            }
        }
    }
    out.push_str("        napi_resolve_deferred(env, ctx->deferred, val);\n");
    out.push_str("    }\n");
    out.push_str("    }\n");
    out.push_str("    free(ctx->err_msg);\n");
    out.push_str("    free(ctx->err_payload);\n");
    match &shape {
        AsyncResultShape::Str | AsyncResultShape::Bytes | AsyncResultShape::Buffered => {
            out.push_str("    free(ctx->result);\n");
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
        emit_ret_out_params(out, &mut c_args, ret);
    }
    c_args.push("&err".to_string());

    let args_str = c_args.join(", ");
    match &f.ret {
        Some(ret) => {
            let rt = c_ret_type_str(ret, module, prefix);
            out.push_str(&format!("  {rt} result = {symbol}({args_str});\n"));
        }
        None => {
            out.push_str(&format!("  {symbol}({args_str});\n"));
        }
    }

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    emit_error_check_c(out, prefix);

    match &f.ret {
        Some(ret) => emit_ret_to_napi(out, ret, prefix, f),
        None => {
            out.push_str("  napi_value ret;\n");
            out.push_str("  napi_get_undefined(env, &ret);\n");
            out.push_str("  return ret;\n");
        }
    }
}

/// Marshal one incoming JS argument into its C ABI slot(s). A buffered
/// parameter arrives as a `Buffer` the JS loader packed; it lowers to the
/// borrowed `(const uint8_t*, size_t)` pair the callee decodes and never
/// frees. Everything else keeps its direct slot lowering.
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
    if is_buffered(ty) {
        out.push_str(&format!("  void* {name}_raw;\n"));
        out.push_str(&format!("  size_t {name}_len;\n"));
        out.push_str(&format!(
            "  napi_get_buffer_info(env, args[{idx}], &{name}_raw, &{name}_len);\n"
        ));
        c_args.push(format!("(const uint8_t*){name}_raw"));
        c_args.push(format!("{name}_len"));
        return;
    }
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            let ct = c_scalar_type(ty);
            let getter = napi_getter(ty);
            out.push_str(&format!("  {ct} {name};\n"));
            out.push_str(&format!("  {getter}(env, args[{idx}], &{name});\n"));
            c_args.push(name.into());
        }
        // N-API has no narrower-than-32-bit / float getter, so read into a
        // correctly-sized temporary and narrow to the real ABI type.
        TypeRef::I8 | TypeRef::I16 | TypeRef::U8 | TypeRef::U16 | TypeRef::U64 | TypeRef::F32 => {
            let ct = c_scalar_type(ty);
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
            let abi_tag = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("({abi_tag}*)(intptr_t){name}_raw"));
        }
        TypeRef::Enum(_) => {
            let etype = abi::lower_param(name, ty, module, false)[0]
                .ty
                .render_c(prefix);
            out.push_str(&format!("  int32_t {name};\n"));
            out.push_str(&format!(
                "  napi_get_value_int32(env, args[{idx}], &{name});\n"
            ));
            c_args.push(format!("({etype}){name}"));
        }
        // An interface arrives as the int64 handle the JS class unwrapped
        // from its instance; the callee borrows the pointer for the call.
        TypeRef::Interface(s) => {
            let abi_tag = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("(const {abi_tag}*)(intptr_t){name}_raw"));
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
        // Only `Interface?` reaches here (every other optional is buffered):
        // JS null/undefined passes NULL, anything else the wrapped handle.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(s) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered");
            };
            let abi_tag = c_abi_struct_name(s, module, prefix);
            out.push_str(&format!("  napi_valuetype {name}_type;\n"));
            out.push_str(&format!("  napi_typeof(env, args[{idx}], &{name}_type);\n"));
            out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str("  }\n");
            c_args.push(format!(
                "{name}_raw ? (const {abi_tag}*)(intptr_t){name}_raw : NULL"
            ));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Declare and thread the trailing out-parameters a return type needs. Bytes
/// and buffered returns share the single `size_t* out_len` slot.
fn emit_ret_out_params(out: &mut String, c_args: &mut Vec<String>, ty: &TypeRef) {
    if is_buffered(ty) || matches!(ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
        out.push_str("  size_t out_len;\n");
        c_args.push("&out_len".into());
    }
}

/// The C statement that creates a napi value `target` from a leaf C expression
/// `expr` (scalars, bools, enums, handles).
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

/// Convert the C `result` (plus `out_len` when present) into the JS return
/// value and release what the consumer owes. A buffered return is copied into
/// a JS `Buffer` and released with `weaveffi_free_bytes`; the JS loader
/// decodes it into the idiomatic value.
fn emit_ret_to_napi(out: &mut String, ty: &TypeRef, prefix: &str, f: &FnBinding) {
    out.push_str("  napi_value ret;\n");
    if is_buffered(ty) {
        out.push_str("  napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
        out.push_str("  weaveffi_free_bytes((uint8_t*)result, out_len);\n");
        out.push_str("  return ret;\n");
        return;
    }
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
        // A returned interface is an owned object reference surfaced as the
        // raw handle; the JS loader wraps it in its class (which owns
        // disposal), so the addon must not destroy it here.
        TypeRef::TypedHandle(_) | TypeRef::Handle | TypeRef::Interface(_) => {
            out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
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
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(_) => {
            out.push_str("  if (result == NULL) {\n");
            out.push_str("    napi_get_null(env, &ret);\n");
            out.push_str("  } else {\n");
            out.push_str("    napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
            out.push_str("  }\n");
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
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str("  return ret;\n");
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

// --- Value-buffer glue ------------------------------------------------------
//
// Buffered values (records, rich enums, optionals, lists, maps, and error
// payloads) cross the addon boundary as `Buffer`s holding the WeaveFFI value
// buffer encoding. The JS loader carries a small private writer/reader
// implementing the wire format, generic combinators for optionals, lists, and
// maps, and one generated pack/unpack function per record and rich enum. The
// composition below is fixed at generation time from the IR; no runtime type
// dispatch happens.

/// The private buffer writer/reader runtime embedded in `index.js` whenever
/// the model uses value buffers. Little-endian, packed, no alignment; decode
/// failures throw the generic error brand (a malformed buffer is a contract
/// violation, not a typed domain error).
const BUFFER_RUNTIME_JS: &str = r#"// --- Private value-buffer runtime (WeaveFFI wire format) --------------------
// Little-endian, packed, no alignment. Decoders reject truncated buffers,
// invalid bool/flag bytes, hostile length prefixes, and trailing bytes.
const __utf8 = new TextDecoder('utf-8', { fatal: true });
function __bufferError(what) {
  return new WeaveFFIError(-2, 'malformed value buffer: ' + what);
}
class __Writer {
  constructor() {
    this._buf = Buffer.alloc(256);
    this._len = 0;
  }
  _reserve(n) {
    if (this._len + n <= this._buf.length) return;
    let cap = this._buf.length;
    while (cap < this._len + n) cap *= 2;
    const grown = Buffer.alloc(cap);
    this._buf.copy(grown, 0, 0, this._len);
    this._buf = grown;
  }
  bool(v) { this._reserve(1); this._buf[this._len++] = v ? 1 : 0; }
  i8(v) { this._reserve(1); this._buf.writeInt8(v, this._len); this._len += 1; }
  u8(v) { this._reserve(1); this._buf.writeUInt8(v, this._len); this._len += 1; }
  i16(v) { this._reserve(2); this._buf.writeInt16LE(v, this._len); this._len += 2; }
  u16(v) { this._reserve(2); this._buf.writeUInt16LE(v, this._len); this._len += 2; }
  i32(v) { this._reserve(4); this._buf.writeInt32LE(v, this._len); this._len += 4; }
  u32(v) { this._reserve(4); this._buf.writeUInt32LE(v, this._len); this._len += 4; }
  i64(v) { this._reserve(8); this._buf.writeBigInt64LE(BigInt(v), this._len); this._len += 8; }
  u64(v) { this._reserve(8); this._buf.writeBigUInt64LE(BigInt(v), this._len); this._len += 8; }
  f32(v) { this._reserve(4); this._buf.writeFloatLE(v, this._len); this._len += 4; }
  f64(v) { this._reserve(8); this._buf.writeDoubleLE(v, this._len); this._len += 8; }
  str(v) {
    const b = Buffer.from(String(v), 'utf8');
    this.u32(b.length);
    this._reserve(b.length);
    b.copy(this._buf, this._len);
    this._len += b.length;
  }
  bytes(v) {
    this.u32(v.length);
    this._reserve(v.length);
    this._buf.set(v, this._len);
    this._len += v.length;
  }
  finish() { return this._buf.subarray(0, this._len); }
}
class __Reader {
  constructor(buf) {
    this._buf = Buffer.isBuffer(buf) ? buf : Buffer.from(buf);
    this._pos = 0;
  }
  _take(n, what) {
    if (this._pos + n > this._buf.length) throw __bufferError(what);
    const at = this._pos;
    this._pos += n;
    return at;
  }
  bool() {
    const b = this._buf[this._take(1, 'bool')];
    if (b > 1) throw __bufferError('bool byte out of range');
    return b === 1;
  }
  i8() { return this._buf.readInt8(this._take(1, 'i8')); }
  u8() { return this._buf.readUInt8(this._take(1, 'u8')); }
  i16() { return this._buf.readInt16LE(this._take(2, 'i16')); }
  u16() { return this._buf.readUInt16LE(this._take(2, 'u16')); }
  i32() { return this._buf.readInt32LE(this._take(4, 'i32')); }
  u32() { return this._buf.readUInt32LE(this._take(4, 'u32')); }
  i64() { return this._buf.readBigInt64LE(this._take(8, 'i64')); }
  u64() { return this._buf.readBigUInt64LE(this._take(8, 'u64')); }
  f32() { return this._buf.readFloatLE(this._take(4, 'f32')); }
  f64() { return this._buf.readDoubleLE(this._take(8, 'f64')); }
  len() {
    const n = this.u32();
    if (n > this._buf.length - this._pos) throw __bufferError('length prefix exceeds remaining bytes');
    return n;
  }
  str() {
    const n = this.len();
    const at = this._take(n, 'string bytes');
    try {
      return __utf8.decode(this._buf.subarray(at, at + n));
    } catch (e) {
      throw __bufferError('string is not valid UTF-8');
    }
  }
  bytes() {
    const n = this.len();
    const at = this._take(n, 'byte buffer');
    return Buffer.from(this._buf.subarray(at, at + n));
  }
  end() {
    if (this._pos !== this._buf.length) throw __bufferError('trailing bytes after value');
  }
}
function __encode(f, v) { const w = new __Writer(); f(w, v); return w.finish(); }
function __decode(f, b) { const r = new __Reader(b); const v = f(r); r.end(); return v; }
function __wOpt(w, v, f) { if (v === null || v === undefined) { w.bool(false); } else { w.bool(true); f(w, v); } }
function __rOpt(r, f) { return r.bool() ? f(r) : null; }
function __wList(w, v, f) { w.u32(v.length); for (const e of v) f(w, e); }
function __rList(r, f) {
  const n = r.len();
  const out = [];
  for (let i = 0; i < n; i++) out.push(f(r));
  return out;
}
function __wMap(w, v, kf, vf) {
  const keys = Object.keys(v);
  w.u32(keys.length);
  for (const k of keys) { kf(w, k); vf(w, v[k]); }
}
function __rMap(r, kf, vf) {
  const n = r.len();
  const out = {};
  for (let i = 0; i < n; i++) {
    const k = kf(r);
    out[k] = vf(r);
  }
  return out;
}

"#;

/// The writer method of the private buffer writer for a leaf type.
fn js_leaf_writer_method(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::Bool => "bool",
        TypeRef::I8 => "i8",
        TypeRef::U8 => "u8",
        TypeRef::I16 => "i16",
        TypeRef::U16 => "u16",
        TypeRef::I32 => "i32",
        TypeRef::U32 => "u32",
        TypeRef::I64 => "i64",
        TypeRef::U64 => "u64",
        TypeRef::F32 => "f32",
        TypeRef::F64 => "f64",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "str",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "bytes",
        _ => unreachable!("not a leaf buffer type"),
    }
}

/// A JS function expression `(w, v) => void` writing one value of `ty` in the
/// wire format. Records and rich enums name their generated pack function;
/// optionals, lists, and maps compose the generic combinators.
fn js_writer_fn(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Enum(_) => "(w, v) => w.i32(v)".into(),
        TypeRef::Handle | TypeRef::TypedHandle(_) => "(w, v) => w.u64(v)".into(),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => format!("__pack{}", local_type_name(n)),
        TypeRef::Optional(inner) => format!("(w, v) => __wOpt(w, v, {})", js_writer_fn(inner)),
        TypeRef::List(inner) => format!("(w, v) => __wList(w, v, {})", js_writer_fn(inner)),
        TypeRef::Map(k, v) => format!(
            "(w, v) => __wMap(w, v, {}, {})",
            js_map_key_writer_fn(k),
            js_writer_fn(v)
        ),
        leaf => format!("(w, v) => w.{}(v)", js_leaf_writer_method(leaf)),
    }
}

/// A JS function expression `(w, k) => void` writing one *map key*. JS object
/// keys arrive as strings from `Object.keys`, so numeric key types coerce
/// through `Number` (or `BigInt`, inside the 64-bit writer methods) first.
fn js_map_key_writer_fn(ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "(w, k) => w.str(k)".into(),
        TypeRef::Bool => "(w, k) => w.bool(k === true || k === 'true')".into(),
        TypeRef::I64 => "(w, k) => w.i64(k)".into(),
        TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => "(w, k) => w.u64(k)".into(),
        TypeRef::Enum(_) => "(w, k) => w.i32(Number(k))".into(),
        leaf => format!("(w, k) => w.{}(Number(k))", js_leaf_writer_method(leaf)),
    }
}

/// A JS function expression `(r) => value` reading one value of `ty` from the
/// wire format. 64-bit integers surface as numbers (matching the TS surface);
/// handles surface as `BigInt`s except typed handles, which keep the numeric
/// handle spelling the addon uses.
fn js_reader_fn(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I64 => "(r) => Number(r.i64())".into(),
        TypeRef::U64 | TypeRef::TypedHandle(_) => "(r) => Number(r.u64())".into(),
        TypeRef::Handle => "(r) => r.u64()".into(),
        TypeRef::Enum(_) => "(r) => r.i32()".into(),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => format!("__unpack{}", local_type_name(n)),
        TypeRef::Optional(inner) => format!("(r) => __rOpt(r, {})", js_reader_fn(inner)),
        TypeRef::List(inner) => format!("(r) => __rList(r, {})", js_reader_fn(inner)),
        TypeRef::Map(k, v) => format!("(r) => __rMap(r, {}, {})", js_reader_fn(k), js_reader_fn(v)),
        leaf => format!("(r) => r.{}()", js_leaf_writer_method(leaf)),
    }
}

/// The JS statement expression writing `val` of type `ty` onto writer `w`.
/// Direct spellings for leaves and generated pack functions; combinator calls
/// for optionals, lists, and maps.
fn js_write_expr(ty: &TypeRef, val: &str) -> String {
    match ty {
        TypeRef::Enum(_) => format!("w.i32({val})"),
        TypeRef::Handle | TypeRef::TypedHandle(_) => format!("w.u64({val})"),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            format!("__pack{}(w, {val})", local_type_name(n))
        }
        TypeRef::Optional(inner) => format!("__wOpt(w, {val}, {})", js_writer_fn(inner)),
        TypeRef::List(inner) => format!("__wList(w, {val}, {})", js_writer_fn(inner)),
        TypeRef::Map(k, v) => format!(
            "__wMap(w, {val}, {}, {})",
            js_map_key_writer_fn(k),
            js_writer_fn(v)
        ),
        leaf => format!("w.{}({val})", js_leaf_writer_method(leaf)),
    }
}

/// The JS expression reading one value of type `ty` from reader `r`.
fn js_read_expr(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I64 => "Number(r.i64())".into(),
        TypeRef::U64 | TypeRef::TypedHandle(_) => "Number(r.u64())".into(),
        TypeRef::Handle => "r.u64()".into(),
        TypeRef::Enum(_) => "r.i32()".into(),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            format!("__unpack{}(r)", local_type_name(n))
        }
        TypeRef::Optional(inner) => format!("__rOpt(r, {})", js_reader_fn(inner)),
        TypeRef::List(inner) => format!("__rList(r, {})", js_reader_fn(inner)),
        TypeRef::Map(k, v) => format!("__rMap(r, {}, {})", js_reader_fn(k), js_reader_fn(v)),
        leaf => format!("r.{}()", js_leaf_writer_method(leaf)),
    }
}

/// True when the loader must embed the buffer runtime: any record or rich
/// enum is declared, any signature position carries a buffered type, or any
/// error code declares payload fields.
fn model_uses_buffers(model: &BindingModel) -> bool {
    model.modules.iter().any(|m| {
        !m.structs.is_empty()
            || m.enums.iter().any(|e| e.is_rich())
            || m.error
                .as_ref()
                .is_some_and(|e| e.declared_here && e.codes.iter().any(|c| !c.fields.is_empty()))
            || m.callbacks
                .iter()
                .any(|cb| cb.params.iter().any(|p| is_buffered(&p.ty)))
            || m.callables().any(|f| {
                f.params.iter().any(|p| is_buffered(&p.ty))
                    || f.ret.as_ref().is_some_and(|t| {
                        is_buffered(t)
                            || matches!(t, TypeRef::Iterator(inner) if is_buffered(inner))
                    })
            })
    })
}

/// Emit one module's pack/unpack functions: one pair per record and one pair
/// per rich enum, with fields written and read in declaration (wire) order.
fn render_pack_fns_js(out: &mut String, m: &ModuleBinding) {
    let mut w = CodeWriter::two_space();
    for s in &m.structs {
        w.block(format!("function __pack{}(w, v) {{", s.name), "}", |w| {
            for field in &s.fields {
                w.line(format!(
                    "{};",
                    js_write_expr(&field.ty, &format!("v.{}", field.name))
                ));
            }
        });
        w.block(format!("function __unpack{}(r) {{", s.name), "}", |w| {
            if s.fields.is_empty() {
                w.line("return {};");
            } else {
                w.line("return {");
                for field in &s.fields {
                    w.line(format!("  {}: {},", field.name, js_read_expr(&field.ty)));
                }
                w.line("};");
            }
        });
    }
    for e in &m.enums {
        if !e.is_rich() {
            continue;
        }
        let name = &e.name;
        // Pack: string tag selects the variant; the i32 discriminant plus the
        // variant's fields go on the wire.
        w.block(format!("function __pack{name}(w, v) {{"), "}", |w| {
            w.block("switch (v.tag) {", "}", |w| {
                for v in &e.variants {
                    w.line(format!("case '{}':", v.name));
                    w.line(format!("  w.i32({});", v.value));
                    for field in &v.fields {
                        w.line(format!(
                            "  {};",
                            js_write_expr(&field.ty, &format!("v.{}", field.name))
                        ));
                    }
                    w.line("  break;");
                }
                w.line("default:");
                w.line(format!(
                    "  throw new {ERROR_BRAND}(-2, 'unknown {name} tag: ' + (v && v.tag));"
                ));
            });
        });
        // Unpack: the i32 discriminant selects the variant; fields decode in
        // order and land next to the string tag.
        w.block(format!("function __unpack{name}(r) {{"), "}", |w| {
            w.line("const tag = r.i32();");
            w.block("switch (tag) {", "}", |w| {
                for v in &e.variants {
                    let fields: String = v
                        .fields
                        .iter()
                        .map(|f| format!(", {}: {}", f.name, js_read_expr(&f.ty)))
                        .collect();
                    w.line(format!(
                        "case {}: return {{ tag: '{}'{fields} }};",
                        v.value, v.name
                    ));
                }
                w.line(format!(
                    "default: throw new {ERROR_BRAND}(-2, 'unknown {name} tag: ' + tag);"
                ));
            });
        });
    }
    let text = w.finish();
    if !text.is_empty() {
        out.push_str(&text);
        out.push('\n');
    }
}

/// Recognize an interface-typed return carried directly or as `Interface?`
/// (the only optional that stays a nullable pointer). Buffered returns are
/// handled separately by the wrapper body.
struct RetWrap {
    /// The local JS class name.
    cls: String,
    /// `true` for `Interface?`: the addon surfaces `null` for the absent case.
    optional: bool,
}

/// Recognize a class-typed (interface) return, direct or optional.
fn js_ret_wrap(ret: Option<&TypeRef>) -> Option<RetWrap> {
    fn direct(ty: &TypeRef, optional: bool) -> Option<RetWrap> {
        match ty {
            TypeRef::Interface(n) => Some(RetWrap {
                cls: local_type_name(n).to_string(),
                optional,
            }),
            _ => None,
        }
    }
    match ret? {
        TypeRef::Optional(inner) => direct(inner, true),
        ty => direct(ty, false),
    }
}

/// The addon-argument expression for one logical parameter. Buffered values
/// pack into a `Buffer` via the generated writer; interface instances unwrap
/// to their raw `_handle` (a borrow; the callee never takes ownership);
/// everything else passes through.
fn js_arg_expr(js_name: &str, ty: &TypeRef) -> String {
    if is_buffered(ty) {
        return format!("__encode({}, {js_name})", js_writer_fn(ty));
    }
    let cls = match ty {
        TypeRef::Interface(n) => Some(local_type_name(n)),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(n) => Some(local_type_name(n)),
            _ => None,
        },
        _ => None,
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
/// (plus the raw payload buffer) to the matching class. Codes that declare
/// payload fields get a decoder that unpacks the buffer and attaches the
/// fields as properties on the error instance; unknown codes fall back to
/// the generic brand (panics and marshalling failures).
fn render_error_classes_js(out: &mut String, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = js_error_factory_name(eb);
    let table = format!("__{}ErrorCodes", eb.owner_path.to_lower_camel_case());
    let payloads = format!("__{}ErrorPayloads", eb.owner_path.to_lower_camel_case());
    let has_payloads = eb.codes.iter().any(|c| !c.fields.is_empty());

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
    if has_payloads {
        // One payload decoder per code that declares fields, reading the
        // code's fields in declaration (wire) order.
        let decoders: Vec<String> = eb
            .codes
            .iter()
            .filter(|c| !c.fields.is_empty())
            .map(|c| {
                let fields: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, js_read_expr(&f.ty)))
                    .collect();
                format!("{}: (r) => ({{ {} }})", c.value, fields.join(", "))
            })
            .collect();
        w.line(format!(
            "const {payloads} = Object.freeze({{ {} }});",
            decoders.join(", ")
        ));
    }
    w.block(
        format!("function {factory}(code, message, payload) {{"),
        "}",
        |w| {
            w.line(format!("const _cls = {table}[code];"));
            w.line(format!(
                "const _err = _cls === undefined ? new {ERROR_BRAND}(code, message) : new _cls(message);"
            ));
            if has_payloads {
                w.line(format!("const _decode = {payloads}[code];"));
                w.block(
                    "if (_decode !== undefined && payload != null) {",
                    "}",
                    |w| {
                        w.line("Object.assign(_err, __decode(_decode, payload));");
                    },
                );
            }
            w.line("return _err;");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Emit one wrapper callable's body: pack buffered arguments and unwrap
/// class-typed ones, invoke the addon binding through the rebranding helper,
/// then decode a buffered result or wrap an interface-typed one.
/// Iterator-returning callables launch the native iterator and hand its
/// external to the shared lazy iterator class, decoding buffered elements per
/// step. Shared by free functions and interface members (`self_expr` supplies
/// the leading handle of an instance method).
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
        // Buffered elements arrive as encoded buffers decoded per step.
        let wrap_elem = if is_buffered(inner) {
            format!("(_e) => __decode({}, _e)", js_reader_fn(inner))
        } else {
            "null".to_string()
        };
        w.line(format!("const _it = {call};"));
        w.line(format!(
            "return new WeaveFFIIterator(_it, addon.{addon_name}_iterNext, addon.{addon_name}_iterDestroy, {map_expr}, {wrap_elem});"
        ));
        return;
    }

    if let Some(ret) = f.ret.as_ref() {
        if is_buffered(ret) {
            let reader = js_reader_fn(ret);
            if f.is_async {
                w.line(format!(
                    "return {call}.then((_r) => __decode({reader}, _r));"
                ));
            } else {
                w.line(format!("const _r = {call};"));
                w.line(format!("return __decode({reader}, _r);"));
            }
            return;
        }
    }

    let Some(wrap) = js_ret_wrap(f.ret.as_ref()) else {
        w.line(format!("return {call};"));
        return;
    };
    let cls = &wrap.cls;
    let rewrap = format!("{cls}._fromHandle(_r)");
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

/// Emit one interface's JS class onto `wv`. The class owns the opaque handle
/// and frees it once, via explicit `destroy()` or a `FinalizationRegistry`
/// safety net. A sync constructor named `new` becomes the JS `constructor`;
/// every other constructor becomes a static factory; methods pass the wrapped
/// handle as the leading addon argument; statics are static methods.
fn render_interface_class_js(
    out: &mut String,
    i: &InterfaceBinding,
    m: &ModuleBinding,
    strip: bool,
) {
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
    model.modules.iter().any(|m| {
        m.callables()
            .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    })
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
        w.block(
            "constructor(ext, nextFn, destroyFn, map, wrapElem) {",
            "}",
            |w| {
                w.line("this._ext = ext;");
                w.line("this._nextFn = nextFn;");
                w.line("this._destroyFn = destroyFn;");
                w.line("this._map = map;");
                w.line("this._wrapElem = wrapElem;");
                w.line("this._done = false;");
            },
        );
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

/// Emit the wrapping `wv.registerX` for a listener whose callback carries
/// buffered arguments: the addon delivers those as borrowed-then-copied
/// `Buffer`s, so the wrapper decodes them before invoking the user's
/// callback. Listeners with only direct arguments keep the plain addon
/// re-export.
fn render_listener_wrapper_js(
    out: &mut String,
    m: &ModuleBinding,
    l: &ListenerBinding,
    strip: bool,
) {
    let Some(cb) = m.callback(&l.event_callback) else {
        return;
    };
    if !cb.params.iter().any(|p| is_buffered(&p.ty)) {
        return;
    }
    let register = js_fn_name(&m.path, &format!("register_{}", l.name), strip);
    let params: Vec<String> = cb.params.iter().map(|p| js_param_name(&p.name)).collect();
    let args: Vec<String> = cb
        .params
        .iter()
        .map(|p| {
            let n = js_param_name(&p.name);
            if is_buffered(&p.ty) {
                format!("__decode({}, {n})", js_reader_fn(&p.ty))
            } else {
                n
            }
        })
        .collect();
    let mut w = CodeWriter::two_space();
    w.block(
        format!("wv.{register} = function (callback) {{"),
        "};",
        |w| {
            w.block(
                format!(
                    "return addon.{register}(function ({}) {{",
                    params.join(", ")
                ),
                "});",
                |w| {
                    w.line(format!("callback({});", args.join(", ")));
                },
            );
        },
    );
    out.push_str(&w.finish());
}

/// The JS loader (`index.js`). Re-exports the native addon's bindings, then
/// layers the idiomatic surface on top: the generic error brand plus one
/// typed error class per declared domain, the private value-buffer runtime
/// with one pack/unpack pair per record and rich enum, wrapper classes for
/// interfaces, and one wrapper per module function so failures rebrand as the
/// right error class and value types cross as plain objects rather than raw
/// buffers.
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
         // (error classes, interface classes, buffer pack/unpack, function\n\
         // wrappers) on top.\n\
         const wv = {};\n\
         for (const _name of Object.getOwnPropertyNames(addon)) {\n  wv[_name] = addon[_name];\n}\n\n",
    );

    // The generic brand and the shared invoke helpers. Every wrapper funnels
    // addon failures (JS errors carrying the numeric ABI `code` and, for
    // structured errors, the raw `payload` buffer) through a mapping factory:
    // the module domain's for throwing callables, the generic constructor
    // otherwise.
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
           return e && typeof e.code === 'number' ? map(e.code, e.message, e.payload) : e;\n\
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

    if model_uses_buffers(model) {
        out.push_str(BUFFER_RUNTIME_JS);
        for m in &model.modules {
            render_pack_fns_js(&mut out, m);
        }
    }

    if model_has_iterators(model) {
        render_iterator_class_js(&mut out);
    }

    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error_classes_js(&mut out, eb);
        }
        for i in &m.interfaces {
            render_interface_class_js(&mut out, i, m, strip);
        }
        for l in &m.listeners {
            render_listener_wrapper_js(&mut out, m, l, strip);
        }
    }

    // One wrapper per module function, so every failure is rebranded and
    // buffered or class-typed values cross as idiomatic values.
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

/// `.d.ts` for a rich (algebraic) enum: a discriminated union with a string
/// `tag` property naming the active variant, plus the variant's fields.
/// Mirrors the tagged-union objects the loader's unpack function produces.
fn render_rich_enum_dts(out: &mut String, e: &EnumBinding) {
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("export type {} =", e.name));
    for v in &e.variants {
        out.push('\n');
        if v.doc.is_some() {
            let mut d = String::new();
            emit_doc(&mut d, &v.doc, "  ");
            out.push_str(&d);
        }
        let fields: String = v
            .fields
            .iter()
            .map(|f| format!("; {}: {}", f.name, ts_type_for(&f.ty)))
            .collect();
        out.push_str(&format!("  | {{ tag: '{}'{fields} }}", v.name));
    }
    out.push_str(";\n");
}

/// `.d.ts` for one declaring module's error surface: the domain class
/// extending the generic brand plus one subclass per code carrying its
/// stable `CODE` and, for codes with structured payloads, the decoded
/// payload fields as readonly properties.
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
        for field in &c.fields {
            emit_doc(out, &field.doc, "  ");
            out.push_str(&format!(
                "  readonly {}: {};\n",
                field.name,
                ts_type_for(&field.ty)
            ));
        }
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
        // Records are plain value types: an interface with typed fields, no
        // handle wrapping, no builders, no destroy.
        for s in &m.structs {
            emit_doc(&mut out, &s.doc, "");
            out.push_str(&format!("export interface {} {{\n", s.name));
            for field in &s.fields {
                emit_doc(&mut out, &field.doc, "  ");
                out.push_str(&format!("  {}: {};\n", field.name, ts_type_for(&field.ty)));
            }
            out.push_str("}\n");
        }
        for e in &m.enums {
            // A rich (algebraic) enum is a tagged union, not a plain numeric
            // `enum`.
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
            version: "0.6.0".into(),
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

    fn field(name: &str, ty: TypeRef) -> StructField {
        StructField {
            name: name.into(),
            ty,
            doc: None,
            default: None,
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

    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>, throws: bool) -> Function {
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

    /// A `Contact { name: string, age: i32 }` record for buffered-type tests.
    fn contact_struct() -> StructDef {
        StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
            ],
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
                params: vec![param("message", TypeRef::StringUtf8)],
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
                field("name", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
                field("active", TypeRef::Bool),
            ],
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
        m.functions.push(func(
            "get_contact",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            false,
        ));
        m.functions.push(func(
            "list_contacts",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
            false,
        ));

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
            m.functions.push(func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
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
                func(
                    "get_contact",
                    vec![param("id", TypeRef::I32)],
                    Some(TypeRef::Optional(Box::new(TypeRef::Record(
                        "Contact".into(),
                    )))),
                    false,
                ),
                func(
                    "list_contacts",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                    false,
                ),
                func(
                    "set_favorite_color",
                    vec![
                        param("contact_id", TypeRef::I32),
                        param(
                            "color",
                            TypeRef::Optional(Box::new(TypeRef::Enum("Color".into()))),
                        ),
                    ],
                    None,
                    false,
                ),
                func(
                    "get_tags",
                    vec![param("contact_id", TypeRef::I32)],
                    Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                    false,
                ),
            ],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    field("name", TypeRef::StringUtf8),
                    field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    field("tags", TypeRef::List(Box::new(TypeRef::StringUtf8))),
                ],
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
            m.functions.push(func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
            m.functions.push(func(
                "subtract",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
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
            m.functions.push(func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
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
            m.functions.push(func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
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
            m.functions.push(func(
                "hello",
                vec![param("name", TypeRef::StringUtf8)],
                Some(TypeRef::StringUtf8),
                false,
            ));
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
            m.functions.push(func(
                "hello",
                vec![param("name", TypeRef::StringUtf8)],
                Some(TypeRef::StringUtf8),
                false,
            ));
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
            m.functions.push(func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
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
            m.functions.push(func(
                "create_contact",
                vec![param("name", TypeRef::StringUtf8)],
                Some(TypeRef::I32),
                false,
            ));
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
            m.structs.push(contact_struct());
            m.functions.push(func(
                "get_info",
                vec![param("contact", TypeRef::TypedHandle("Contact".into()))],
                None,
                false,
            ));
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
        let api = make_api(vec![{
            let mut m = make_module("edge");
            m.structs.push(contact_struct());
            m.functions.push(func(
                "process",
                vec![param(
                    "data",
                    TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Record("Contact".into())),
                    ))))),
                )],
                None,
                false,
            ));
            m
        }]);
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("(Contact | null)[] | null"),
            "should contain deeply nested optional type: {dts}"
        );
    }

    #[test]
    fn node_map_of_lists() {
        let api = make_api(vec![{
            let mut m = make_module("edge");
            m.functions.push(func(
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
            ));
            m
        }]);
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("Record<string, number[]>"),
            "should contain map of lists type: {dts}"
        );
    }

    #[test]
    fn node_enum_keyed_map() {
        let api = make_api(vec![{
            let mut m = make_module("edge");
            m.structs.push(contact_struct());
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
                ],
            });
            m.functions.push(func(
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
            ));
            m
        }]);
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("Record<Color, Contact>"),
            "should contain enum-keyed map type: {dts}"
        );
        // The wrapper packs the map with an enum key writer (JS object keys
        // arrive as strings, so the key coerces through Number) and the
        // record's pack function per value.
        let index = index_for(&api, true);
        assert!(
            index.contains(
                "__encode((w, v) => __wMap(w, v, (w, k) => w.i32(Number(k)), __packContact), contacts)"
            ),
            "map param must pack keys and values: {index}"
        );
    }

    #[test]
    fn node_no_double_free_on_error() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(contact_struct());
            m.functions.push(func(
                "find_contact",
                vec![param("name", TypeRef::StringUtf8)],
                Some(TypeRef::Record("Contact".into())),
                false,
            ));
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
        // The buffered record return is copied into a JS Buffer, then the
        // native encoding is released exactly once.
        assert!(
            addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
            "buffered return must be freed after copying: {addon}"
        );
    }

    #[test]
    fn node_null_check_on_optional_interface_return() {
        // `Interface?` is the one optional that stays a nullable pointer at
        // the ABI (every other optional is buffered), so the addon must
        // null-check before surfacing the handle.
        let api = make_api(vec![{
            let mut m = make_module("kv");
            m.interfaces.push(InterfaceDef {
                name: "Store".into(),
                doc: None,
                constructors: vec![func("new", vec![], None, false)],
                methods: vec![],
                statics: vec![],
            });
            m.functions.push(func(
                "maybe_open",
                vec![param("path", TypeRef::StringUtf8)],
                Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                    "Store".into(),
                )))),
                false,
            ));
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("if (result == NULL)"),
            "optional interface return should null-check before wrapping: {addon}"
        );
        assert!(
            addon.contains("napi_get_null"),
            "optional absent should return JS null via napi_get_null: {addon}"
        );
        let index = index_for(&api, true);
        assert!(
            index.contains("return _r == null ? null : Store._fromHandle(_r);"),
            "the wrapper must null-check before wrapping the handle: {index}"
        );
    }

    #[test]
    fn node_async_returns_promise() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.functions.push(Function {
                name: "run".into(),
                params: vec![param("id", TypeRef::I32)],
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
                params: vec![param("id", TypeRef::I32)],
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
        // A rejection carries the copied structured payload.
        assert!(
            addon.contains("ctx->err_payload = (uint8_t*)malloc(err->payload_len)"),
            "the error payload must be copied inside the callback: {addon}"
        );
        assert!(
            addon.contains(
                "weaveffi_napi_error_value(env, ctx->err_code, ctx->err_msg, ctx->err_payload, ctx->err_payload_len)"
            ),
            "the rejection must carry the copied payload: {addon}"
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
                params: vec![param("id", TypeRef::I32)],
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

    // --- Value buffers -------------------------------------------------------

    #[test]
    fn buffer_runtime_emitted_only_when_needed() {
        // A scalar-only model carries no buffer runtime.
        let plain = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
            ));
            m
        }]);
        let index = index_for(&plain, true);
        assert!(
            !index.contains("class __Writer"),
            "scalar-only model must not embed the buffer runtime: {index}"
        );

        // Declaring a record pulls in the writer, reader, and combinators.
        let buffered = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(contact_struct());
            m.functions.push(func(
                "save",
                vec![param("contact", TypeRef::Record("Contact".into()))],
                None,
                false,
            ));
            m
        }]);
        let index = index_for(&buffered, true);
        for piece in [
            "class __Writer",
            "class __Reader",
            "function __wOpt",
            "function __rList",
            "function __wMap",
        ] {
            assert!(
                index.contains(piece),
                "buffer runtime piece `{piece}` missing: {index}"
            );
        }
    }

    #[test]
    fn record_params_and_returns_use_value_buffers() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(contact_struct());
            m.functions.push(func(
                "save",
                vec![param("contact", TypeRef::Record("Contact".into()))],
                Some(TypeRef::Record("Contact".into())),
                false,
            ));
            m
        }]);

        // Addon: the record param crosses as the borrowed (ptr, len) pair the
        // JS layer packed; the return is an owned encoding freed after the
        // copy into a JS Buffer.
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("napi_get_buffer_info(env, args[0], &contact_raw, &contact_len);"),
            "buffered param must read the packed Buffer: {addon}"
        );
        assert!(
            addon.contains(
                "const uint8_t* result = weaveffi_contacts_save((const uint8_t*)contact_raw, contact_len, &out_len, &err);"
            ),
            "the call must pass ptr+len and thread out_len: {addon}"
        );
        assert!(
            addon.contains("napi_create_buffer_copy(env, out_len, result, NULL, &ret);"),
            "the buffered return must be copied into a JS Buffer: {addon}"
        );
        assert!(
            addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
            "the owned encoding must be freed with weaveffi_free_bytes: {addon}"
        );
        // Records have no native helpers at all.
        assert!(
            !addon.contains("Contact_get_") && !addon.contains("Contact_destroy"),
            "records must not have native getters or destructors: {addon}"
        );

        // Loader: generated pack/unpack write fields in declaration order and
        // the wrapper encodes the argument and decodes the result.
        let index = index_for(&api, true);
        assert!(
            index.contains("function __packContact(w, v) {"),
            "missing pack function: {index}"
        );
        let name_write = index.find("w.str(v.name);").expect("pack writes name");
        let age_write = index.find("w.i32(v.age);").expect("pack writes age");
        assert!(
            name_write < age_write,
            "fields must pack in declaration order: {index}"
        );
        assert!(
            index.contains("function __unpackContact(r) {")
                && index.contains("name: r.str(),")
                && index.contains("age: r.i32(),"),
            "missing unpack function: {index}"
        );
        assert!(
            index.contains(
                "const _r = __invoke(addon.save, [__encode(__packContact, contact)], __generic);"
            ),
            "the wrapper must pack the record argument: {index}"
        );
        assert!(
            index.contains("return __decode(__unpackContact, _r);"),
            "the wrapper must decode the record result: {index}"
        );
    }

    #[test]
    fn optional_record_return_is_buffered() {
        // `Contact?` is buffered (the absence flag lives inside the buffer),
        // so the addon must not null-check the pointer; the JS layer decodes
        // the flag byte instead.
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(contact_struct());
            m.functions.push(func(
                "find",
                vec![param("id", TypeRef::I32)],
                Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                false,
            ));
            m
        }]);
        let addon = addon_for(&api, true);
        assert!(
            !addon.contains("if (result == NULL)"),
            "buffered optional must not null-check the pointer: {addon}"
        );
        assert!(
            addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
            "buffered optional return must be freed: {addon}"
        );
        let index = index_for(&api, true);
        assert!(
            index.contains("return __decode((r) => __rOpt(r, __unpackContact), _r);"),
            "the wrapper must decode through the optional combinator: {index}"
        );
    }

    #[test]
    fn async_buffered_result_copied_then_decoded() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.structs.push(contact_struct());
            m.functions.push(Function {
                name: "fetch_contact".into(),
                params: vec![],
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            });
            m
        }]);

        // The completion callback receives a BORROWED buffer: it must copy
        // the bytes before returning (the producer frees them afterwards).
        let addon = addon_for(&api, true);
        assert!(
            addon.contains(
                "static void weaveffi_tasks_fetch_contact_napi_cb(void* context, weaveffi_error* err, const uint8_t* result_ptr, size_t result_len) {"
            ),
            "callback must take the borrowed buffer slots: {addon}"
        );
        assert!(
            addon.contains(
                "ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result_ptr, result_len);"
            ),
            "callback must deep-copy the borrowed buffer: {addon}"
        );
        assert!(
            addon.contains("napi_create_buffer_copy(env, ctx->result_len,"),
            "settle must surface the copied bytes as a JS Buffer: {addon}"
        );

        // The JS wrapper decodes the resolved buffer.
        let index = index_for(&api, true);
        assert!(
            index.contains(
                "return __invokeAsync(addon.fetchContact, [], __generic).then((_r) => __decode(__unpackContact, _r));"
            ),
            "the async wrapper must decode the resolved buffer: {index}"
        );
    }

    #[test]
    fn iterator_buffered_elements_decoded_and_freed() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(contact_struct());
            m.functions.push(func(
                "iter_contacts",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                false,
            ));
            m
        }]);

        // Addon: `_next` pulls the encoded element plus its length, copies it
        // into a JS Buffer, then releases it with weaveffi_free_bytes.
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("size_t iter_item_len = 0;"),
            "buffered elements need the extra length slot: {addon}"
        );
        assert!(
            addon.contains("&iter_item, &iter_item_len, &iter_err"),
            "next must thread the element length out-param: {addon}"
        );
        assert!(
            addon.contains("napi_create_buffer_copy(env, iter_item_len, iter_item, NULL, &ret);"),
            "the element must be copied into a JS Buffer: {addon}"
        );
        assert!(
            addon.contains("weaveffi_free_bytes((uint8_t*)iter_item, iter_item_len);"),
            "the element encoding must be freed after copying: {addon}"
        );

        // Loader: the lazy iterator decodes each element buffer per step.
        let index = index_for(&api, true);
        assert!(
            index.contains(
                "return new WeaveFFIIterator(_it, addon.iterContacts_iterNext, addon.iterContacts_iterDestroy, __generic, (_e) => __decode(__unpackContact, _e));"
            ),
            "the iterator wrapper must decode each element: {index}"
        );
    }

    #[test]
    fn error_payload_fields_decoded_and_attached() {
        let api = make_api(vec![{
            let mut m = make_module("kv");
            m.functions.push(func(
                "get",
                vec![param("key", TypeRef::StringUtf8)],
                Some(TypeRef::StringUtf8),
                true,
            ));
            m.errors = Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    ErrorCode {
                        name: "KeyNotFound".into(),
                        code: 1001,
                        message: "key not found".into(),
                        doc: None,
                        fields: vec![
                            field("key", TypeRef::StringUtf8),
                            field("attempts", TypeRef::I32),
                        ],
                    },
                    ErrorCode {
                        name: "StoreFull".into(),
                        code: 1003,
                        message: "store is full".into(),
                        doc: None,
                        fields: vec![],
                    },
                ],
            });
            m
        }]);

        // Addon: the native error helper attaches the raw payload buffer.
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("napi_set_named_property(env, err, \"payload\", payload_val);"),
            "the error helper must attach the payload buffer: {addon}"
        );
        assert!(
            addon.contains(
                "napi_throw(env, weaveffi_napi_error_value(env, err.code, err.message, err.payload_ptr, err.payload_len));"
            ),
            "the sync throw must pass the payload slots: {addon}"
        );

        // Loader: codes with fields get a payload decoder; the factory
        // attaches the decoded fields as properties on the error.
        let index = index_for(&api, true);
        assert!(
            index.contains(
                "const __kvErrorPayloads = Object.freeze({ 1001: (r) => ({ key: r.str(), attempts: r.i32() }) });"
            ),
            "missing the per-code payload decoders: {index}"
        );
        assert!(
            index.contains("function __kvErrorFrom(code, message, payload) {"),
            "the factory must accept the payload buffer: {index}"
        );
        assert!(
            index.contains("Object.assign(_err, __decode(_decode, payload));"),
            "decoded payload fields must land as error properties: {index}"
        );

        // Declarations: the payload fields surface as readonly properties on
        // the code's error class.
        let dts = dts_for(&api, true);
        assert!(
            dts.contains("export class KeyNotFoundError extends KvError {"),
            "missing per-code class: {dts}"
        );
        assert!(
            dts.contains("  readonly key: string;") && dts.contains("  readonly attempts: number;"),
            "payload fields must be declared on the class: {dts}"
        );
    }

    #[test]
    fn listener_buffered_params_decoded() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![contact_struct()],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnContact".into(),
                doc: None,
                params: vec![param("contact", TypeRef::Record("Contact".into()))],
            }],
            listeners: vec![ListenerDef {
                name: "contact_listener".into(),
                event_callback: "OnContact".into(),
                doc: None,
            }],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        // Addon: the borrowed (ptr, len) argument is deep-copied by the
        // trampoline, surfaced as a JS Buffer by the marshaller, then freed.
        let addon = addon_for(&api, true);
        assert!(
            addon.contains("uint8_t* contact_ptr;") && addon.contains("size_t contact_len;"),
            "the payload struct must own the copied buffer: {addon}"
        );
        assert!(
            addon.contains(
                "if (contact_ptr != NULL && contact_len > 0) { p->contact_ptr = (uint8_t*)malloc(contact_len); memcpy(p->contact_ptr, contact_ptr, contact_len); }"
            ),
            "the trampoline must deep-copy the borrowed buffer: {addon}"
        );
        assert!(
            addon.contains("napi_create_buffer_copy(env, p->contact_len,"),
            "the marshaller must surface the copied buffer: {addon}"
        );
        assert!(
            addon.contains("free(p->contact_ptr);"),
            "the payload copy must be freed after the JS call: {addon}"
        );

        // Loader: the register wrapper decodes the buffer before invoking the
        // user's callback.
        let index = index_for(&api, true);
        assert!(
            index.contains("wv.registerContactListener = function (callback) {"),
            "missing the register wrapper: {index}"
        );
        assert!(
            index.contains("callback(__decode(__unpackContact, contact));"),
            "the wrapper must decode the buffered argument: {index}"
        );

        // Declarations type the callback in terms of the record.
        let dts = dts_for(&api, true);
        assert!(
            dts.contains(
                "export function registerContactListener(callback: (contact: Contact) => void): number"
            ),
            "register dts must type the record param: {dts}"
        );
    }

    // --- Rich (algebraic) enum support ------------------------------------

    /// A module mirroring `samples/shapes/shapes.yml`: a rich enum `Shape`
    /// (unit + f64 + two-f32 + string/u8 variants), a plain enum `Channel`, and
    /// the free functions that take/return the rich enum plus a numeric smoke.
    fn shapes_module() -> Module {
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
                func(
                    "describe",
                    vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                    Some(TypeRef::StringUtf8),
                    false,
                ),
                func(
                    "scale",
                    vec![
                        param("shape", TypeRef::RichEnum("Shape".into())),
                        param("factor", TypeRef::F64),
                    ],
                    Some(TypeRef::RichEnum("Shape".into())),
                    false,
                ),
                func(
                    "sum_bytes",
                    vec![param("values", TypeRef::List(Box::new(TypeRef::U8)))],
                    Some(TypeRef::U64),
                    false,
                ),
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
    fn rich_enum_addon_marshals_value_buffers() {
        let addon = addon_for(&make_api(vec![shapes_module()]), false);

        // Rich enums are value types: no tag reader, no per-variant
        // constructors or getters, no destructor.
        for gone in [
            "Shape_tag",
            "Shape_Empty_new",
            "Shape_Circle_new",
            "Shape_Circle_get_radius",
            "Shape_destroy",
        ] {
            assert!(
                !addon.contains(gone),
                "rich enums must have no native helper {gone}: {addon}"
            );
        }

        // Free functions marshal the rich enum as a value buffer, in and out.
        assert!(
            addon.contains("napi_get_buffer_info(env, args[0], &shape_raw, &shape_len);"),
            "describe must read the packed shape buffer: {addon}"
        );
        assert!(
            addon.contains("weaveffi_shapes_describe((const uint8_t*)shape_raw, shape_len, &err);"),
            "describe must pass the borrowed ptr+len pair: {addon}"
        );
        assert!(
            addon.contains(
                "const uint8_t* result = weaveffi_shapes_scale((const uint8_t*)shape_raw, shape_len, factor, &out_len, &err);"
            ),
            "scale must take a buffer and return an owned one: {addon}"
        );
        assert!(
            addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
            "the returned encoding must be freed: {addon}"
        );

        // A list<u8> parameter is buffered too.
        assert!(
            addon.contains(
                "weaveffi_shapes_sum_bytes((const uint8_t*)values_raw, values_len, &err);"
            ),
            "list params must cross as value buffers: {addon}"
        );
    }

    #[test]
    fn rich_enum_index_js_packs_tagged_unions() {
        let index = index_for(&make_api(vec![shapes_module()]), false);

        // Pack: the string tag selects the variant, then the i32 discriminant
        // plus the variant's fields go on the wire in order.
        assert!(
            index.contains("function __packShape(w, v) {"),
            "missing pack function: {index}"
        );
        assert!(
            index.contains("case 'Circle':") && index.contains("w.i32(1);"),
            "circle variant must pack its discriminant: {index}"
        );
        assert!(
            index.contains("w.f64(v.radius);"),
            "circle variant must pack its field: {index}"
        );
        assert!(
            index.contains("w.f32(v.width);") && index.contains("w.f32(v.height);"),
            "rectangle variant must pack both f32 fields: {index}"
        );
        assert!(
            index.contains("w.str(v.label);") && index.contains("w.u8(v.count);"),
            "labeled variant must pack string + u8: {index}"
        );
        // An unknown tag is a caller bug surfaced as the generic brand.
        assert!(
            index.contains("throw new WeaveFFIError(-2, 'unknown Shape tag: ' + (v && v.tag));"),
            "pack must reject unknown tags: {index}"
        );

        // Unpack: the i32 discriminant selects the variant; fields land next
        // to the string tag.
        assert!(
            index.contains("function __unpackShape(r) {"),
            "missing unpack function: {index}"
        );
        assert!(
            index.contains("case 0: return { tag: 'Empty' };"),
            "unit variant must unpack to a bare tag: {index}"
        );
        assert!(
            index.contains("case 1: return { tag: 'Circle', radius: r.f64() };"),
            "circle variant must unpack its field: {index}"
        );
        assert!(
            index.contains("case 3: return { tag: 'Labeled', label: r.str(), count: r.u8() };"),
            "labeled variant must unpack in field order: {index}"
        );
        assert!(
            index.contains("default: throw new WeaveFFIError(-2, 'unknown Shape tag: ' + tag);"),
            "unpack must reject unknown discriminants: {index}"
        );

        // Wrappers pack arguments and decode results; no classes, no handles.
        assert!(
            index.contains("wv.shapesScale = function (shape, factor) {")
                && index.contains(
                    "const _r = __invoke(addon.shapesScale, [__encode(__packShape, shape), factor], __generic);"
                )
                && index.contains("return __decode(__unpackShape, _r);"),
            "scale must pack its argument and decode its result: {index}"
        );
        assert!(
            index.contains(
                "return __invoke(addon.shapesDescribe, [__encode(__packShape, shape)], __generic);"
            ),
            "describe must pack its argument: {index}"
        );
        assert!(
            !index.contains("class Shape"),
            "rich enums must not surface as classes: {index}"
        );
    }

    #[test]
    fn index_js_without_domains_wraps_with_generic_brand() {
        // Even with no rich enums, interfaces, or error domains, every
        // function gets a wrapper so a non-zero error slot (panic or
        // marshalling failure) surfaces as the generic brand class.
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
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
    fn rich_enum_dts_emits_tagged_union() {
        let dts = dts_for(&make_api(vec![shapes_module()]), false);

        // Rich enum -> a discriminated union keyed by a string tag.
        assert!(
            dts.contains("export type Shape ="),
            "rich enum must be a union type: {dts}"
        );
        assert!(
            !dts.contains("export enum Shape") && !dts.contains("export class Shape"),
            "rich enum must not be a plain enum or a class: {dts}"
        );
        assert!(dts.contains("| { tag: 'Empty' }"), "{dts}");
        assert!(dts.contains("| { tag: 'Circle'; radius: number }"), "{dts}");
        assert!(
            dts.contains("| { tag: 'Rectangle'; width: number; height: number }"),
            "{dts}"
        );
        assert!(
            dts.contains("| { tag: 'Labeled'; label: string; count: number }"),
            "{dts}"
        );

        // Plain enum still surfaces as a numeric `enum`.
        assert!(
            dts.contains("export enum Channel {"),
            "plain enum stays an enum: {dts}"
        );

        // Free functions are typed in terms of the union; unstripped names
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
                        fields: vec![],
                    },
                    ErrorCode {
                        name: "StoreFull".into(),
                        code: 1003,
                        message: "store is full".into(),
                        doc: None,
                        fields: vec![],
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

        // Every failure path throws the code-and-payload-carrying error object.
        assert!(
            addon.contains(
                "napi_throw(env, weaveffi_napi_error_value(env, err.code, err.message, err.payload_ptr, err.payload_len));"
            ),
            "sync errors must carry the ABI code and payload: {addon}"
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
                "napi_throw(env, weaveffi_napi_error_value(env, iter_err.code, iter_err.message, iter_err.payload_ptr, iter_err.payload_len));"
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

        // The method wrapper launches (packing the optional prefix into a
        // value buffer), then hands the external to the class with its
        // per-iterator next/destroy bindings and error mapping.
        assert!(
            index.contains(
                "const _it = __invoke(addon.Store_list_keys, [this._handle, __encode((w, v) => __wOpt(w, v, (w, v) => w.str(v)), prefix)], __kvErrorFrom);"
            ),
            "the wrapper must pack the optional param and launch: {index}"
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
        // The factory maps a raw code (plus the raw payload buffer) to the
        // matching class and falls back to the generic brand for unknown
        // codes (panics, marshalling).
        assert!(
            index.contains("function __kvErrorFrom(code, message, payload) {"),
            "missing domain factory: {index}"
        );
        assert!(
            index.contains("1001: KeyNotFoundError, 1003: StoreFullError"),
            "missing code table: {index}"
        );
        assert!(
            index.contains(
                "const _err = _cls === undefined ? new WeaveFFIError(code, message) : new _cls(message);"
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
