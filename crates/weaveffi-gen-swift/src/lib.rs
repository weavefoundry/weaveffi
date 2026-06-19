//! Swift binding generator for WeaveFFI.
//!
//! Emits a SwiftPM package containing a thin Swift wrapper over the C ABI,
//! including module map, `Package.swift`, and Swift `async/await` shims for
//! functions marked `async: true`. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, walk_modules, DocCommentStyle};
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, FieldBinding, FnBinding, ListenerBinding, ModuleBinding,
    ParamBinding, RichVariantBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, Module, TypeRef};

/// Per-target configuration for [`SwiftGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SwiftConfig {
    /// SwiftPM module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// When `true`, strip the IR module name prefix from emitted function
    /// names (e.g. `add` instead of `math_add`).
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the Swift wrappers call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    /// Populated by the CLI; not user-configurable via `[swift]`.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl SwiftConfig {
    /// Returns the configured SwiftPM module name, falling back to
    /// `"WeaveFFI"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("WeaveFFI")
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

/// Each module contributes ~2KB of Swift wrapper text on average (struct
/// shims, getters, async wrappers); pre-allocating from this estimate
/// reduces `String` re-allocations as the wrapper grows past 64 KB.
const SWIFT_BASE_BYTES: usize = 4096;
const SWIFT_BYTES_PER_MODULE: usize = 2048;
const SWIFT_BYTES_PER_FUNCTION: usize = 512;
const SWIFT_BYTES_PER_STRUCT: usize = 512;

fn estimate_swift_capacity(modules: &[Module]) -> usize {
    fn count(modules: &[Module]) -> (usize, usize, usize) {
        let mut m = 0;
        let mut f = 0;
        let mut s = 0;
        for module in modules {
            m += 1;
            f += module.functions.len();
            s += module.structs.len();
            let (sm, sf, ss) = count(&module.modules);
            m += sm;
            f += sf;
            s += ss;
        }
        (m, f, s)
    }
    let (mods, funcs, structs) = count(modules);
    SWIFT_BASE_BYTES
        + mods * SWIFT_BYTES_PER_MODULE
        + funcs * SWIFT_BYTES_PER_FUNCTION
        + structs * SWIFT_BYTES_PER_STRUCT
}

/// Swift backend: emits a SwiftPM package with a thin Swift wrapper (module
/// map, `Package.swift`, and `async`/`await` shims) over the C ABI.
pub struct SwiftGenerator;

impl LanguageBackend for SwiftGenerator {
    type Config = SwiftConfig;

    fn name(&self) -> &'static str {
        "swift"
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
        // SwiftPM package/module name: an explicit `[swift] module_name`
        // wins; otherwise the IDL `package:` name (PascalCased to a legal
        // Swift module) drives it; falling back to the `WeaveFFI` brand.
        let module_name_owned = config
            .module_name
            .clone()
            .or_else(|| api.package.as_ref().map(|p| p.name.to_upper_camel_case()))
            .unwrap_or_else(|| "WeaveFFI".to_string());
        let module_name = module_name_owned.as_str();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("swift");
        let c_module = format!("C{module_name}");
        // The C shim is a SwiftPM `systemLibrary` target, so its module map
        // must live under `Sources/<target>/` for `swift build` to find it.
        let module_dir = dir.join("Sources").join(&c_module);

        let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
        // `swift-tools-version` MUST be the very first line of the manifest
        // (Swift 6+ rejects it otherwise), so the WeaveFFI header prelude
        // follows it rather than preceding it.
        let package = format!(
            "// swift-tools-version:5.7\n\
{prelude}import PackageDescription\n\n\
let package = Package(\n    \
    name: \"{name}\",\n    \
    platforms: [.macOS(.v10_15), .iOS(.v13), .tvOS(.v13), .watchOS(.v6)],\n    \
    products: [\n        \
        .library(name: \"{name}\", targets: [\"{name}\"]),\n    \
    ],\n    \
    targets: [\n        \
        .systemLibrary(name: \"{c_name}\"),\n        \
        .target(name: \"{name}\", dependencies: [\"{c_name}\"]),\n    \
    ]\n\
)\n\n\
{trailer}",
            name = module_name,
            c_name = c_module,
            trailer = render_trailer(CommentStyle::DoubleSlash, "Package.swift"),
        );

        // The module map lives at `swift/Sources/C<module>/module.modulemap`,
        // so the C header generated at `<out>/c/<prefix>.h` is three levels up.
        let modulemap = format!(
            "{prelude}module {} [system] {{\n  header \"../../../c/{prefix}.h\"\n  link \"weaveffi\"\n  export *\n}}\n\n{trailer}",
            c_module,
            trailer = render_trailer(CommentStyle::DoubleSlash, "module.modulemap"),
        );

        let src_dir = dir.join("Sources").join(module_name);
        let swift_filename = format!("{module_name}.swift");
        vec![
            OutputFile::new(dir.join("Package.swift"), package),
            OutputFile::new(module_dir.join("module.modulemap"), modulemap),
            OutputFile::new(
                src_dir.join(&swift_filename),
                render_swift_wrapper(
                    api,
                    prefix,
                    config.strip_module_prefix,
                    input_basename,
                    &swift_filename,
                ),
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
        let module_name_owned = config
            .module_name
            .clone()
            .or_else(|| api.package.as_ref().map(|p| p.name.to_upper_camel_case()))
            .unwrap_or_else(|| "WeaveFFI".to_string());
        let module_name = module_name_owned.as_str();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("swift");
        let c_module = format!("C{module_name}");
        let xcframework = format!("{c_module}.xcframework");

        let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
        // The packaged manifest consumes a prebuilt `binaryTarget` xcframework
        // instead of a `systemLibrary`, so installation needs no system lib on
        // the search path.
        let package_swift = format!(
            "// swift-tools-version:5.7\n\
{prelude}import PackageDescription\n\n\
let package = Package(\n    \
    name: \"{name}\",\n    \
    platforms: [.macOS(.v10_15), .iOS(.v13), .tvOS(.v13), .watchOS(.v6)],\n    \
    products: [\n        \
        .library(name: \"{name}\", targets: [\"{name}\"]),\n    \
    ],\n    \
    targets: [\n        \
        .binaryTarget(name: \"{c_name}\", path: \"{xcframework}\"),\n        \
        .target(name: \"{name}\", dependencies: [\"{c_name}\"]),\n    \
    ]\n\
)\n\n\
{trailer}",
            name = module_name,
            c_name = c_module,
            xcframework = xcframework,
            trailer = render_trailer(CommentStyle::DoubleSlash, "Package.swift"),
        );

        let src_dir = dir.join("Sources").join(module_name);
        let swift_filename = format!("{module_name}.swift");
        let wrapper = render_swift_wrapper(
            api,
            prefix,
            config.strip_module_prefix,
            input_basename,
            &swift_filename,
        );

        let mut files = vec![
            PackagedFile::text(dir.join("Package.swift"), package_swift),
            PackagedFile::text(src_dir.join(&swift_filename), wrapper),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(module_name, &c_module, prefix, ctx, input_basename),
            ),
        ];
        // Bundle the prebuilt libraries as xcframework-ready slices.
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(SwiftGenerator);

/// README for a packaged Swift artifact: it documents assembling the
/// `binaryTarget` xcframework from the bundled per-platform slices, the one
/// step that requires Apple tooling (`lipo` + `xcodebuild`).
fn render_packaged_readme(
    module_name: &str,
    c_module: &str,
    prefix: &str,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {module_name} (Swift)

A SwiftPM package whose C ABI is consumed through a prebuilt `binaryTarget`
xcframework named `{c_module}.xcframework`.

The prebuilt libraries are bundled under `lib/<platform>/`. Assembling them into
an xcframework is the one step that needs Apple tooling (run on macOS):

```bash
# Fuse the macOS arm64 and x86_64 dylibs into one universal binary.
lipo -create \
  lib/darwin-arm64/lib{prefix}.dylib \
  lib/darwin-x64/lib{prefix}.dylib \
  -output lib{prefix}.dylib

# Headers/ must contain {prefix}.h and a module map naming the module {c_module}.
mkdir -p Headers
cp ../c/include/{prefix}.h Headers/
printf 'module {c_module} {{\n  header "{prefix}.h"\n  export *\n}}\n' > Headers/module.modulemap

xcodebuild -create-xcframework \
  -library lib{prefix}.dylib -headers Headers \
  -output {c_module}.xcframework
```

Then `swift build` resolves the binary target with no further setup.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}

/// Emits a `///`-prefixed Swift doc comment at `indent`. Each line of the
/// (possibly multi-line) doc gets its own `///` prefix.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::TripleSlash);
}

/// Emits Swift doc comments for a function: the function's own doc followed by
/// `/// - Parameter name: ...` lines for each documented parameter.
fn emit_fn_doc(out: &mut String, doc: &Option<String>, params: &[ParamBinding], indent: &str) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    if doc.is_none() && !has_param_docs {
        return;
    }
    emit_doc(out, doc, indent);
    for p in params {
        if let Some(pdoc) = &p.doc {
            let pdoc = pdoc.trim();
            if pdoc.is_empty() {
                continue;
            }
            let mut lines = pdoc.lines();
            if let Some(first) = lines.next() {
                out.push_str(indent);
                out.push_str(&format!(
                    "/// - Parameter {}: {}\n",
                    p.name.to_lower_camel_case(),
                    first
                ));
            }
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str("///\n");
                } else {
                    out.push_str("///   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
}

fn swift_type_for(t: &TypeRef) -> String {
    match t {
        TypeRef::I8 => "Int8".to_string(),
        TypeRef::I16 => "Int16".to_string(),
        TypeRef::I32 => "Int32".to_string(),
        TypeRef::U8 => "UInt8".to_string(),
        TypeRef::U16 => "UInt16".to_string(),
        TypeRef::U32 => "UInt32".to_string(),
        TypeRef::U64 => "UInt64".to_string(),
        TypeRef::I64 => "Int64".to_string(),
        TypeRef::F32 => "Float".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Data".to_string(),
        TypeRef::Handle => "UInt64".to_string(),
        TypeRef::TypedHandle(name) | TypeRef::Enum(name) => local_type_name(name).to_string(),
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{}?", swift_type_for(inner)),
        TypeRef::List(inner) => format!("[{}]", swift_type_for(inner)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_for(k), swift_type_for(v)),
        TypeRef::Iterator(inner) => format!("[{}]", swift_type_for(inner)),
    }
}

/// Context threaded into the function/return renderers so they can emit the
/// fully-prefixed C symbols (for iterators) and disambiguate wrapper types that
/// collide with a module namespace.
#[derive(Clone, Copy)]
struct SwiftCtx<'a> {
    /// C ABI symbol prefix (e.g. `weaveffi`).
    c_prefix: &'a str,
    /// SwiftPM module name (e.g. `Kvstore`).
    swift_module: &'a str,
    /// Every module name in the API, PascalCased, i.e. the set of namespace
    /// `enum` names that wrapper-type references can be shadowed by.
    module_names: &'a HashSet<String>,
}

impl SwiftCtx<'_> {
    /// Qualify a top-level wrapper type name with the Swift module when its
    /// name collides with a namespace `enum`. Inside `enum Kv { enum Stats { … } }`
    /// the bare name `Stats` resolves to the namespace, not the top-level
    /// `class Stats`; `Kvstore.Stats` forces the class. Module-qualifying is
    /// valid from any scope, so we apply it whenever the name collides.
    fn ty_name(&self, local: &str) -> String {
        if self.module_names.contains(local) {
            format!("{}.{}", self.swift_module, local)
        } else {
            local.to_string()
        }
    }
}

/// Like [`swift_type_for`] but disambiguates wrapper-type names that collide
/// with a module namespace (see [`SwiftCtx::ty_name`]).
fn swift_type_ctx(t: &TypeRef, ctx: SwiftCtx) -> String {
    match t {
        TypeRef::TypedHandle(name) | TypeRef::Struct(name) | TypeRef::Enum(name) => {
            ctx.ty_name(local_type_name(name))
        }
        TypeRef::Optional(inner) => format!("{}?", swift_type_ctx(inner, ctx)),
        TypeRef::List(inner) => format!("[{}]", swift_type_ctx(inner, ctx)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_ctx(k, ctx), swift_type_ctx(v, ctx)),
        TypeRef::Iterator(inner) => format!("[{}]", swift_type_ctx(inner, ctx)),
        _ => swift_type_for(t),
    }
}

fn is_c_value_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::U64
            | TypeRef::I64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_)
    )
}

/// True for `string`/`borrowed_str` directly or wrapped in `optional`. These
/// marshal to a NUL-terminated `const char*` via `withCString`, distinct from
/// `bytes` (which pass an explicit `(ptr, len)` pair).
fn is_string_shaped(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => true,
        TypeRef::Optional(inner) => {
            matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr)
        }
        _ => false,
    }
}

fn needs_closure(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::List(_)
        | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => is_c_value_type(inner) || is_string_shaped(ty),
        _ => false,
    }
}

fn has_buffer_params(params: &[ParamBinding]) -> bool {
    params.iter().any(|p| needs_closure(&p.ty))
}

fn render_swift_enum(out: &mut String, e: &EnumBinding) {
    emit_doc(out, &e.doc, "");
    // Match how Swift imports the generated C enum: a C enum with only
    // non-negative discriminants is imported with a `UInt32` raw value,
    // otherwise `Int32`. Mirroring the raw type here keeps every `.rawValue`
    // round-trip against the C symbols type-correct (the C getters return, and
    // the C constructors accept, that same unsigned/signed width).
    let raw = if e.variants.iter().any(|v| v.value < 0) {
        "Int32"
    } else {
        "UInt32"
    };
    out.push_str(&format!("public enum {}: {} {{\n", e.name, raw));
    for v in &e.variants {
        emit_doc(out, &v.doc, "    ");
        out.push_str(&format!(
            "    case {} = {}\n",
            v.name.to_lower_camel_case(),
            v.value
        ));
    }
    out.push_str("}\n\n");
}

/// Render a rich (algebraic) enum as an opaque-object wrapper class, mirroring
/// the struct wrapper: it owns the C handle (`let ptr: OpaquePointer`), frees it
/// in `deinit` via the enum's `_destroy`, exposes a nested `Tag: Int32`
/// discriminant plus a `tag` reader, a throwing static factory per variant
/// (`Shape.circle(2.0)`), and per-variant field getters namespaced by variant
/// (`circleRadius`) so identically-named fields on different variants never
/// collide. Functions that take or return the enum see it lowered to
/// [`TypeRef::Struct`], so the existing `x.ptr` / `T(ptr:)` marshalling binds
/// them with no special-casing.
fn render_swift_rich_enum(
    out: &mut String,
    c_prefix: &str,
    module_path: &str,
    e: &EnumBinding,
    ctx: SwiftCtx,
) {
    let Some(rich) = &e.rich else {
        return;
    };
    let class_name = &e.name;

    emit_doc(out, &e.doc, "");
    out.push_str(&format!("public class {} {{\n", class_name));
    out.push_str("    let ptr: OpaquePointer\n\n");
    out.push_str("    init(ptr: OpaquePointer) {\n");
    out.push_str("        self.ptr = ptr\n");
    out.push_str("    }\n\n");
    out.push_str(&format!(
        "    deinit {{\n        {}(ptr)\n    }}\n\n",
        rich.destroy_symbol
    ));

    // The C tag getter returns `int32_t`, so the nested discriminant enum is
    // always `Int32`-backed (regardless of the variant value signs).
    out.push_str("    public enum Tag: Int32 {\n");
    for v in &e.variants {
        emit_doc(out, &v.doc, "        ");
        out.push_str(&format!(
            "        case {} = {}\n",
            v.name.to_lower_camel_case(),
            v.value
        ));
    }
    out.push_str("    }\n\n");

    out.push_str("    /// The active variant's discriminant.\n");
    out.push_str("    public var tag: Tag {\n");
    out.push_str(&format!(
        "        return Tag(rawValue: {}(ptr))!\n",
        rich.tag_symbol
    ));
    out.push_str("    }\n\n");

    for v in &rich.variants {
        render_swift_rich_variant_factory(out, c_prefix, module_path, class_name, v, ctx);
    }

    // Getters are namespaced per variant (`circleRadius`) and reuse the struct
    // field getter marshalling unchanged: the field's `getter_symbol` already
    // encodes the per-variant C accessor, only the Swift property name differs.
    for v in &rich.variants {
        for f in &v.fields {
            let mut named = f.clone();
            named.name = format!(
                "{}{}",
                v.name.to_lower_camel_case(),
                f.name.to_upper_camel_case()
            );
            render_swift_getter(out, &named, ctx);
        }
    }

    out.push_str("}\n\n");
}

/// One throwing static factory for a rich-enum variant (`static func
/// circle(_ radius: Double) throws -> Shape`). Reuses the struct `create`
/// marshalling: a buffer-free variant calls its `_new` symbol directly, while a
/// variant carrying a string/bytes/list/map payload threads the same
/// `withCString`/buffer staging the struct builder uses.
fn render_swift_rich_variant_factory(
    out: &mut String,
    c_prefix: &str,
    module_path: &str,
    class_name: &str,
    v: &RichVariantBinding,
    ctx: SwiftCtx,
) {
    let params = struct_fields_as_params(&v.fields);
    let create_sym = &v.create.symbol;

    emit_doc(out, &v.doc, "    ");
    let _ = write!(
        out,
        "    public static func {}(",
        v.name.to_lower_camel_case()
    );
    write_swift_params_sig(out, &params, ctx);
    let _ = writeln!(out, ") throws -> {} {{", class_name);
    out.push_str("        var err = weaveffi_error(code: 0, message: nil)\n");

    if !has_buffer_params(&params) {
        let call_args = build_c_call_args(&params, c_prefix, module_path);
        if call_args.is_empty() {
            let _ = writeln!(out, "        let ptr = {}(&err)", create_sym);
        } else {
            let _ = writeln!(out, "        let ptr = {}({}, &err)", create_sym, call_args);
        }
        out.push_str("        try check(&err)\n");
        out.push_str(
            "        guard let ptr = ptr else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n",
        );
        let _ = writeln!(out, "        return {}(ptr: ptr)", class_name);
    } else {
        render_buffered_struct_create(out, c_prefix, module_path, create_sym, &params, class_name);
    }

    out.push_str("    }\n\n");
}

fn render_swift_wrapper(
    api: &Api,
    c_prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let mut out = String::with_capacity(estimate_swift_capacity(&api.modules));
    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    // The C shim target is `C<module_name>` and the wrapper file is always
    // `<module_name>.swift`, so the system-library module to import is the
    // file stem with a `C` prefix. Deriving it here keeps the `import` in sync
    // with the module name picked from `[swift] module_name` / the IDL package.
    let module_name = filename.strip_suffix(".swift").unwrap_or(filename);
    out.push_str(&format!("import C{module_name}\nimport Foundation\n\n"));

    let model = BindingModel::build(api, c_prefix);
    // Index the flat, pre-order model by its underscore-joined symbol path so
    // the recursive IR walk below can pull each module's precomputed C symbols
    // while still emitting the nested Swift `enum` structure the IR tree drives.
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    let all_mods = walk_modules(&api.modules).collect::<Vec<_>>();
    let error_codes: Vec<_> = all_mods
        .iter()
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();

    // Every module becomes a namespace `enum`; a wrapper type whose name
    // matches one of these is shadowed inside that namespace and must be
    // module-qualified at its use sites.
    let module_names: HashSet<String> = all_mods
        .iter()
        .map(|m| m.name.to_upper_camel_case())
        .collect();
    let ctx = SwiftCtx {
        c_prefix,
        swift_module: module_name,
        module_names: &module_names,
    };

    out.push_str("public enum WeaveFFIError: Error, LocalizedError {\n");
    out.push_str("    case error(code: Int32, message: String)\n");
    for ec in &error_codes {
        emit_doc(&mut out, &ec.doc, "    ");
        out.push_str(&format!("    case {}\n", ec.name.to_lower_camel_case()));
    }
    out.push_str("    public var errorDescription: String? {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(_, message): return message\n");
    for ec in &error_codes {
        out.push_str(&format!(
            "        case .{}: return \"{}\"\n",
            ec.name.to_lower_camel_case(),
            ec.message
        ));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    public var errorCode: Int32 {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(code, _): return code\n");
    for ec in &error_codes {
        out.push_str(&format!(
            "        case .{}: return {}\n",
            ec.name.to_lower_camel_case(),
            ec.code
        ));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc check(_ err: inout weaveffi_error) throws {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    if error_codes.is_empty() {
        out.push_str("        throw WeaveFFIError.error(code: code, message: message)\n");
    } else {
        out.push_str("        switch code {\n");
        for ec in &error_codes {
            out.push_str(&format!(
                "        case {}: throw WeaveFFIError.{}\n",
                ec.code,
                ec.name.to_lower_camel_case()
            ));
        }
        out.push_str("        default: throw WeaveFFIError.error(code: code, message: message)\n");
        out.push_str("        }\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc withOptionalPointer<T, R>(to value: T?, _ body: (UnsafePointer<T>?) throws -> R) rethrows -> R {\n");
    out.push_str("    guard let value = value else { return try body(nil) }\n");
    out.push_str("    return try withUnsafePointer(to: value) { try body($0) }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc withOptionalCString<R>(_ value: String?, _ body: (UnsafePointer<CChar>?) throws -> R) rethrows -> R {\n");
    out.push_str("    guard let value = value else { return try body(nil) }\n");
    out.push_str("    return try value.withCString { try body($0) }\n");
    out.push_str("}\n\n");

    let has_async = all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    if has_async {
        out.push_str("private final class ContinuationRef<T> {\n");
        out.push_str("    let value: CheckedContinuation<T, Error>\n");
        out.push_str("    init(_ value: CheckedContinuation<T, Error>) { self.value = value }\n");
        out.push_str("}\n\n");
    }

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        // A C function pointer cannot capture state, so each registered Swift
        // closure is boxed and threaded through the `void* context` slot. The
        // registry keeps the +1 retain alive until unregistration releases it.
        out.push_str("final class WvCallbackBox<T> {\n");
        out.push_str("    let value: T\n");
        out.push_str("    init(_ value: T) { self.value = value }\n");
        out.push_str("}\n\n");
        out.push_str("var wvListenerContexts: [UInt64: UnsafeMutableRawPointer] = [:]\n");
        out.push_str("let wvListenerLock = NSLock()\n\n");
    }

    for m in &api.modules {
        render_swift_module_types(&mut out, c_prefix, &by_path, m, &m.name, ctx);
        let type_name = m.name.to_upper_camel_case();
        out.push_str(&format!("public enum {} {{\n", type_name));
        render_swift_module_body(
            &mut out,
            c_prefix,
            &by_path,
            m,
            &m.name,
            1,
            strip_module_prefix,
            ctx,
        );
        out.push_str("}\n\n");
    }
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

fn render_swift_module_types(
    out: &mut String,
    c_prefix: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
    m: &Module,
    module_path: &str,
    ctx: SwiftCtx,
) {
    let mb = by_path[module_path];
    for e in &mb.enums {
        // A rich (algebraic) enum is an opaque object, emitted as a wrapper
        // class alongside structs; only a plain C-style enum maps to a Swift
        // `enum`.
        if e.is_rich() {
            render_swift_rich_enum(out, c_prefix, module_path, e, ctx);
        } else {
            render_swift_enum(out, e);
        }
    }
    for s in &mb.structs {
        render_swift_struct(out, s, ctx);
        if s.builder.is_some() {
            render_swift_builder(out, c_prefix, module_path, s);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_swift_module_types(out, c_prefix, by_path, sub, &sub_path, ctx);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_swift_module_body(
    out: &mut String,
    c_prefix: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
    m: &Module,
    module_path: &str,
    depth: usize,
    strip_module_prefix: bool,
    ctx: SwiftCtx,
) {
    let indent = "    ".repeat(depth);
    let mb = by_path[module_path];
    let mut bodies: Vec<String> = Vec::new();
    for l in &mb.listeners {
        let mut buf = String::new();
        render_swift_listener(&mut buf, module_path, mb, l, strip_module_prefix, ctx);
        bodies.push(buf);
    }
    for f in &mb.functions {
        let mut buf = String::new();
        if f.is_async {
            render_swift_async_function(
                &mut buf,
                c_prefix,
                module_path,
                f,
                strip_module_prefix,
                ctx,
            );
        } else {
            render_swift_function(&mut buf, c_prefix, module_path, f, strip_module_prefix, ctx);
        }
        bodies.push(buf);
    }
    for buf in bodies {
        if depth > 1 {
            let extra = "    ".repeat(depth - 1);
            for line in buf.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(&extra);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(&buf);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        let sub_name = sub.name.to_upper_camel_case();
        out.push_str(&format!("{indent}public enum {sub_name} {{\n"));
        render_swift_module_body(
            out,
            c_prefix,
            by_path,
            sub,
            &sub_path,
            depth + 1,
            strip_module_prefix,
            ctx,
        );
        out.push_str(&format!("{indent}}}\n"));
    }
}

fn render_swift_struct(out: &mut String, s: &StructBinding, ctx: SwiftCtx) {
    let prefix = &s.c_tag;

    emit_doc(out, &s.doc, "");
    out.push_str(&format!("public class {} {{\n", s.name));
    out.push_str("    let ptr: OpaquePointer\n\n");
    out.push_str("    init(ptr: OpaquePointer) {\n");
    out.push_str("        self.ptr = ptr\n");
    out.push_str("    }\n\n");
    out.push_str(&format!(
        "    deinit {{\n        {}_destroy(ptr)\n    }}\n",
        prefix
    ));

    for field in &s.fields {
        render_swift_getter(out, field, ctx);
    }

    out.push_str("}\n\n");
}

fn struct_fields_as_params(fields: &[FieldBinding]) -> Vec<ParamBinding> {
    fields
        .iter()
        .map(|f| ParamBinding {
            name: f.name.clone(),
            ty: f.ty.clone(),
            mutable: false,
            doc: f.doc.clone(),
            abi: vec![],
        })
        .collect()
}

fn render_swift_builder(out: &mut String, c_prefix: &str, module_name: &str, s: &StructBinding) {
    let prefix = &s.c_tag;
    let class_name = local_type_name(&s.name);
    let builder_name = format!("{class_name}Builder");

    emit_doc(out, &s.doc, "");
    out.push_str(&format!("public class {} {{\n", builder_name));
    for field in &s.fields {
        let swift_ty = swift_type_for(&field.ty);
        out.push_str(&format!("    private var _{}: {}?\n", field.name, swift_ty));
    }
    out.push_str("\n    public init() {}\n\n");

    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let swift_ty = swift_type_for(&field.ty);
        emit_doc(out, &field.doc, "    ");
        out.push_str("    @discardableResult\n");
        out.push_str(&format!(
            "    public func with{}(_ value: {}) -> Self {{\n        self._{} = value\n        return self\n    }}\n\n",
            pascal, swift_ty, field.name
        ));
    }

    let params = struct_fields_as_params(&s.fields);
    out.push_str(&format!(
        "    public func build() throws -> {} {{\n",
        class_name
    ));
    for field in &s.fields {
        out.push_str(&format!(
            "        guard let {} = _{} else {{ fatalError(\"missing field: {}\") }}\n",
            field.name, field.name, field.name
        ));
    }
    out.push_str("        var err = weaveffi_error(code: 0, message: nil)\n");

    if !has_buffer_params(&params) {
        let create_sym = format!("{}_create", prefix);
        let call_args = build_c_call_args(&params, c_prefix, module_name);
        if call_args.is_empty() {
            out.push_str(&format!("        let ptr = {}(&err)\n", create_sym));
        } else {
            out.push_str(&format!(
                "        let ptr = {}({}, &err)\n",
                create_sym, call_args
            ));
        }
        out.push_str("        try check(&err)\n");
        out.push_str(
            "        guard let ptr = ptr else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n",
        );
        out.push_str(&format!("        return {}(ptr: ptr)\n", class_name));
    } else {
        let create_sym = format!("{}_create", prefix);
        render_buffered_struct_create(out, c_prefix, module_name, &create_sym, &params, class_name);
    }

    out.push_str("    }\n}\n\n");
}

fn render_swift_getter(out: &mut String, field: &FieldBinding, ctx: SwiftCtx) {
    let getter = &field.getter_symbol;
    let swift_ty = swift_type_for(&field.ty);

    out.push('\n');
    emit_doc(out, &field.doc, "    ");
    out.push_str(&format!("    public var {}: {} {{\n", field.name, swift_ty));

    match &field.ty {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("        let raw = {}(ptr)\n", getter));
            out.push_str("        guard let raw = raw else { return \"\" }\n");
            out.push_str("        defer { weaveffi_free_string(raw) }\n");
            out.push_str("        return String(cString: raw)\n");
        }
        TypeRef::Bytes => {
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!("        let raw = {}(ptr, &outLen)\n", getter));
            out.push_str("        guard let raw = raw else { return Data() }\n");
            out.push_str("        defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: raw), outLen) }\n");
            out.push_str("        return Data(bytes: raw, count: outLen)\n");
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("        return {}(ptr: {}(ptr)!)\n", name, getter));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str("        guard let p = p else { return nil }\n");
                out.push_str("        defer { weaveffi_free_string(p) }\n");
                out.push_str("        return String(cString: p)\n");
            }
            TypeRef::Bytes => {
                out.push_str("        var outLen: Int = 0\n");
                out.push_str(&format!("        let p = {}(ptr, &outLen)\n", getter));
                out.push_str("        guard let p = p else { return nil }\n");
                out.push_str("        defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: p), outLen) }\n");
                out.push_str("        return Data(bytes: p, count: outLen)\n");
            }
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let name = local_type_name(name);
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str(&format!("        return p.map {{ {}(ptr: $0) }}\n", name));
            }
            TypeRef::Enum(name) => {
                let name = local_type_name(name);
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str(&format!(
                    "        return p.map {{ {}(rawValue: $0.pointee.rawValue)! }}\n",
                    name
                ));
            }
            _ if is_c_value_type(inner) => {
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str("        return p?.pointee\n");
            }
            _ => {
                out.push_str(&format!("        return {}(ptr)\n", getter));
            }
        },
        TypeRef::List(inner) => {
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!("        let rv = {}(ptr, &outLen)\n", getter));
            out.push_str("        guard let rv = rv else { return [] }\n");
            match inner.as_ref() {
                TypeRef::Enum(name) => {
                    let name = local_type_name(name);
                    out.push_str(&format!(
                        "        return (0..<outLen).map {{ {}(rawValue: rv[$0].rawValue)! }}\n",
                        name
                    ));
                }
                TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                    let name = local_type_name(name);
                    out.push_str(&format!(
                        "        return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                        name
                    ));
                }
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str("        return (0..<outLen).map { String(cString: rv[$0]!) }\n");
                }
                _ => {
                    out.push_str(
                        "        return Array(UnsafeBufferPointer(start: rv, count: outLen))\n",
                    );
                }
            }
        }
        TypeRef::Map(k, v) => {
            let key_elem = swift_c_ptr_element(k);
            let val_elem = swift_c_ptr_element(v);
            let key_swift = swift_type_for(k);
            let val_swift = swift_type_for(v);
            out.push_str(&format!(
                "        var outKeysPtr: UnsafeMutablePointer<{}>? = nil\n",
                key_elem
            ));
            out.push_str(&format!(
                "        var outValuesPtr: UnsafeMutablePointer<{}>? = nil\n",
                val_elem
            ));
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!(
                "        {}(ptr, &outKeysPtr, &outValuesPtr, &outLen)\n",
                getter
            ));
            out.push_str(
                "        guard let outKeys = outKeysPtr, let outValues = outValuesPtr else { return [:] }\n",
            );
            out.push_str(&format!(
                "        var result: [{}: {}] = [:]\n",
                key_swift, val_swift
            ));
            out.push_str("        for i in 0..<outLen {\n");
            let key_expr = map_element_read(k, "outKeys[i]", ctx);
            let val_expr = map_element_read(v, "outValues[i]", ctx);
            out.push_str(&format!(
                "            result[{}] = {}\n",
                key_expr, val_expr
            ));
            out.push_str("        }\n");
            out.push_str("        return result\n");
        }
        TypeRef::Enum(name) => {
            let name = local_type_name(name);
            out.push_str(&format!(
                "        return {}(rawValue: {}(ptr).rawValue)!\n",
                name, getter
            ));
        }
        _ => {
            out.push_str(&format!("        return {}(ptr)\n", getter));
        }
    }

    out.push_str("    }\n");
}

fn render_swift_function(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    strip_module_prefix: bool,
    ctx: SwiftCtx,
) {
    emit_fn_doc(out, &f.doc, &f.params, "    ");
    if let Some(msg) = &f.deprecated {
        let _ = writeln!(
            out,
            "    @available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        );
    }
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let ret_swift = f
        .ret
        .as_ref()
        .map(|t| swift_type_ctx(t, ctx))
        .unwrap_or_else(|| "Void".to_string());
    let _ = write!(out, "    public static func {}(", func_name);
    write_swift_params_sig(out, &f.params, ctx);
    let _ = writeln!(out, ") throws -> {} {{", ret_swift);
    out.push_str("        var err = weaveffi_error(code: 0, message: nil)\n");

    let c_sym = &f.c_base;
    let call_args = build_c_call_args(&f.params, c_prefix, module_name);
    let call_with_err = if call_args.is_empty() {
        format!("{}(&err)", c_sym)
    } else {
        format!("{}({}, &err)", c_sym, call_args)
    };

    if !has_buffer_params(&f.params) {
        render_direct_call(out, f, &call_with_err, ctx);
    } else {
        render_buffered_call(out, c_prefix, f, &f.params, module_name, ctx);
    }

    out.push_str("    }\n");
}

/// Write `_ name: SwiftType, _ name: SwiftType, ...` directly into `out`,
/// avoiding the per-call `format!` and intermediate `Vec<String>` allocations
/// that `params.iter().map(format!).collect::<Vec<_>>().join(", ")` would
/// require.
fn write_swift_params_sig(out: &mut String, params: &[ParamBinding], ctx: SwiftCtx) {
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "_ {}: {}", p.name, swift_type_ctx(&p.ty, ctx));
    }
}

/// The Swift type one callback parameter surfaces as in the user closure.
/// Struct and handle parameters stay raw (`OpaquePointer?`): wrapping them in
/// the owning Swift class would `*_destroy` a borrowed handle on ARC release.
fn swift_cb_param_type(ty: &TypeRef, ctx: SwiftCtx) -> String {
    match ty {
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => "OpaquePointer?".into(),
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) =>
        {
            "OpaquePointer?".into()
        }
        other => swift_type_ctx(other, ctx),
    }
}

/// The expression converting one callback parameter's C slots into the value
/// handed to the user closure. Slot names follow [`abi::lower_param`].
fn swift_cb_arg_expr(p: &ParamBinding, ctx: SwiftCtx) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = slots[0].name.clone();
    match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::I64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => n0,
        TypeRef::Bool => format!("{n0} != 0"),
        TypeRef::Enum(name) => {
            let local = swift_type_ctx(&p.ty, ctx);
            let _ = name;
            format!("{local}(rawValue: {n0}.rawValue)!")
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("String(cString: {n0}!)"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = &slots[1].name;
            format!(
                "{n0} != nil ? [UInt8](UnsafeBufferPointer(start: {n0}, count: Int({n1}))) : []"
            )
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => n0,
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("{n0}.map {{ String(cString: $0) }}")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let n1 = &slots[1].name;
                format!("{n0}.map {{ [UInt8](UnsafeBufferPointer(start: $0, count: Int({n1}))) }}")
            }
            TypeRef::Enum(_) => {
                let local = swift_type_ctx(inner, ctx);
                format!("{n0}.map {{ {local}(rawValue: $0.pointee.rawValue)! }}")
            }
            TypeRef::Bool => format!("{n0}.map {{ $0.pointee != 0 }}"),
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => n0,
            _ => format!("{n0}.map {{ $0.pointee }}"),
        },
        TypeRef::List(inner) => {
            let n1 = &slots[1].name;
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!(
                    "{n0} != nil ? (0..<Int({n1})).map {{ String(cString: {n0}![$0]!) }} : []"
                ),
                TypeRef::Enum(_) => {
                    let local = swift_type_ctx(inner, ctx);
                    format!(
                        "{n0} != nil ? (0..<Int({n1})).map {{ {local}(rawValue: {n0}![$0].rawValue)! }} : []"
                    )
                }
                TypeRef::Bool => {
                    format!("{n0} != nil ? (0..<Int({n1})).map {{ {n0}![$0] != 0 }} : []")
                }
                _ => {
                    let elem = swift_type_ctx(inner, ctx);
                    format!(
                        "{n0} != nil ? [{elem}](UnsafeBufferPointer(start: {n0}, count: Int({n1}))) : []"
                    )
                }
            }
        }
        TypeRef::Map(k, v) => {
            let keys = &slots[0].name;
            let vals = &slots[1].name;
            let len = &slots[2].name;
            let key_expr = swift_map_elem_expr(k, keys, ctx);
            let val_expr = swift_map_elem_expr(v, vals, ctx);
            format!(
                "Dictionary(uniqueKeysWithValues: (0..<Int({len})).map {{ ({key_expr}, {val_expr}) }})"
            )
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
}

/// One element read from a parallel-array base pointer at closure index `$0`.
fn swift_map_elem_expr(ty: &TypeRef, base: &str, ctx: SwiftCtx) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("String(cString: {base}![$0]!)"),
        TypeRef::Enum(_) => {
            let local = swift_type_ctx(ty, ctx);
            format!("{local}(rawValue: {base}![$0].rawValue)!")
        }
        TypeRef::Bool => format!("{base}![$0] != 0"),
        _ => format!("{base}![$0]"),
    }
}

/// The register/unregister pair for one listener. The user closure is boxed
/// (`WvCallbackBox`) and retained through the C `context` pointer; the
/// capture-free trampoline closure unboxes and invokes it.
fn render_swift_listener(
    out: &mut String,
    module_path: &str,
    mb: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
    ctx: SwiftCtx,
) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_fn = wrapper_name(
        module_path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    let unregister_fn = wrapper_name(
        module_path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    );

    let closure_type = format!(
        "({}) -> Void",
        cb.params
            .iter()
            .map(|p| swift_cb_param_type(&p.ty, ctx))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Trampoline closure formals: every ABI slot, context last.
    let slot_names: Vec<String> = cb.abi_params.iter().map(|s| s.name.clone()).collect();
    let args: Vec<String> = cb
        .params
        .iter()
        .map(|p| swift_cb_arg_expr(p, ctx))
        .collect();

    emit_fn_doc(out, &l.doc, &[], "    ");
    let _ = writeln!(
        out,
        "    /// - Returns: A subscription id for ``{unregister_fn}(_:)``."
    );
    let _ = writeln!(
        out,
        "    public static func {register_fn}(_ callback: @escaping {closure_type}) -> UInt64 {{"
    );
    out.push_str("        let box = WvCallbackBox(callback)\n");
    out.push_str("        let ctx = Unmanaged.passRetained(box).toOpaque()\n");
    let _ = writeln!(
        out,
        "        let id = {}({{ {} in",
        l.register_symbol,
        slot_names.join(", ")
    );
    let _ = writeln!(
        out,
        "            let cb = Unmanaged<WvCallbackBox<{closure_type}>>.fromOpaque(context!).takeUnretainedValue().value"
    );
    let _ = writeln!(out, "            cb({})", args.join(", "));
    out.push_str("        }, ctx)\n");
    out.push_str("        wvListenerLock.lock()\n");
    out.push_str("        wvListenerContexts[id] = ctx\n");
    out.push_str("        wvListenerLock.unlock()\n");
    out.push_str("        return id\n");
    out.push_str("    }\n");

    let _ = writeln!(
        out,
        "    /// Unregisters a listener previously registered with ``{register_fn}(_:)``."
    );
    let _ = writeln!(
        out,
        "    public static func {unregister_fn}(_ id: UInt64) {{"
    );
    let _ = writeln!(out, "        {}(id)", l.unregister_symbol);
    out.push_str("        wvListenerLock.lock()\n");
    out.push_str("        let ctx = wvListenerContexts.removeValue(forKey: id)\n");
    out.push_str("        wvListenerLock.unlock()\n");
    out.push_str("        if let ctx = ctx {\n");
    let _ = writeln!(
        out,
        "            Unmanaged<WvCallbackBox<{closure_type}>>.fromOpaque(ctx).release()"
    );
    out.push_str("        }\n");
    out.push_str("    }\n");
}

fn render_swift_async_function(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    strip_module_prefix: bool,
    ctx: SwiftCtx,
) {
    emit_fn_doc(out, &f.doc, &f.params, "    ");
    if let Some(msg) = &f.deprecated {
        let _ = writeln!(
            out,
            "    @available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        );
    }
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let ret_swift = f
        .ret
        .as_ref()
        .map(|t| swift_type_ctx(t, ctx))
        .unwrap_or_else(|| "Void".to_string());

    let _ = write!(out, "    public static func {}(", func_name);
    write_swift_params_sig(out, &f.params, ctx);
    let _ = writeln!(out, ") async throws -> {} {{", ret_swift);
    let _ = writeln!(
        out,
        "        try await withCheckedThrowingContinuation {{ (continuation: CheckedContinuation<{}, Error>) in",
        ret_swift
    );
    out.push_str(
        "            let ctx = Unmanaged.passRetained(ContinuationRef(continuation)).toOpaque()\n",
    );

    let base = "            ";

    for p in &f.params {
        match &p.ty {
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("{}let {n}_bytes = Array({n})\n", base, n = p.name));
            }
            TypeRef::Optional(inner) => {
                if let TypeRef::Enum(enum_name) = inner.as_ref() {
                    out.push_str(&format!(
                        "{}let {n}_c: {c_prefix}_{m}_{e}? = {n}.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
            }
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::Enum(enum_name) => {
                    out.push_str(&format!(
                        "{}let {n}_raw = {n}.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!(
                        "{}let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        base,
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::Map(k, v) => {
                out.push_str(&format!(
                    "{}let {n}_keys = Array({n}.keys)\n",
                    base,
                    n = p.name
                ));
                out.push_str(&format!(
                    "{}let {n}_values = {n}_keys.map {{ {n}[$0]! }}\n",
                    base,
                    n = p.name
                ));
                if let TypeRef::Enum(e) = k.as_ref() {
                    out.push_str(&format!(
                        "{}let {n}_keysRaw = {n}_keys.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = e
                    ));
                } else if matches!(k.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) {
                    out.push_str(&format!(
                        "{}let {n}_keysPtrs = {n}_keys.map {{ $0.ptr }}\n",
                        base,
                        n = p.name
                    ));
                }
                if let TypeRef::Enum(e) = v.as_ref() {
                    out.push_str(&format!(
                        "{}let {n}_valuesRaw = {n}_values.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = e
                    ));
                } else if matches!(v.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) {
                    out.push_str(&format!(
                        "{}let {n}_valuesPtrs = {n}_values.map {{ $0.ptr }}\n",
                        base,
                        n = p.name
                    ));
                }
            }
            _ => {}
        }
        stage_cstring_arrays(out, base, p);
    }

    let closure_params: Vec<&ParamBinding> =
        f.params.iter().filter(|p| needs_closure(&p.ty)).collect();
    let mut closure_depth: usize = 0;

    for p in &closure_params {
        let indent = format!("{}{}", base, "    ".repeat(closure_depth));
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!(
                    "{}{}.withCString {{ {}_ptr in\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner)
                if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
            {
                out.push_str(&format!(
                    "{}withOptionalCString({}) {{ {}_ptr in\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!(
                    "{}{}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress!\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                out.push_str(&format!(
                    "{}withOptionalPointer(to: {}) {{ {}_ptr in\n",
                    indent, source, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::List(inner) => {
                let source = list_array_source(inner, &p.name);
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Map(k, v) => {
                let keys_source = map_array_source(k, &p.name, "keys");
                let values_source = map_array_source(v, &p.name, "values");
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                    indent, keys_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_keys_ptr = {}_keys_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
                let vind = format!("{}{}", base, "    ".repeat(closure_depth));
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_values_buf in\n",
                    vind, values_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_values_ptr = {}_values_buf.baseAddress\n",
                    vind, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_values_buf.count\n",
                    vind, p.name, p.name
                ));
                closure_depth += 1;
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = format!("{}{}", base, "    ".repeat(closure_depth));
    let c_sym = format!("{}_async", f.c_base);
    let call_args = build_c_call_args(&f.params, c_prefix, module_name);
    let cb_param_names = async_callback_param_names(&f.ret);

    if f.cancellable {
        if call_args.is_empty() {
            out.push_str(&format!(
                "{}{}(nil, {{ {} in\n",
                inner_indent, c_sym, cb_param_names
            ));
        } else {
            out.push_str(&format!(
                "{}{}({}, nil, {{ {} in\n",
                inner_indent, c_sym, call_args, cb_param_names
            ));
        }
    } else if call_args.is_empty() {
        out.push_str(&format!(
            "{}{}({{ {} in\n",
            inner_indent, c_sym, cb_param_names
        ));
    } else {
        out.push_str(&format!(
            "{}{}({}, {{ {} in\n",
            inner_indent, c_sym, call_args, cb_param_names
        ));
    }

    let cb_indent = format!("{}    ", inner_indent);
    out.push_str(&format!(
        "{}let contRef = Unmanaged<ContinuationRef<{}>>.fromOpaque(context!).takeRetainedValue()\n",
        cb_indent, ret_swift
    ));
    out.push_str(&format!(
        "{}if let err = err, err.pointee.code != 0 {{\n",
        cb_indent
    ));
    out.push_str(&format!("{}    let code = err.pointee.code\n", cb_indent));
    out.push_str(&format!(
        "{}    let msg = err.pointee.message.flatMap {{ String(cString: $0) }} ?? \"\"\n",
        cb_indent
    ));
    out.push_str(&format!(
        "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: code, message: msg))\n",
        cb_indent
    ));
    out.push_str(&format!("{}}} else {{\n", cb_indent));

    let success_indent = format!("{}    ", cb_indent);
    render_async_resume_result(
        out,
        c_prefix,
        &f.ret,
        &success_indent,
        module_name,
        &f.name,
        ctx,
    );

    out.push_str(&format!("{}}}\n", cb_indent));
    out.push_str(&format!("{}}}, ctx)\n", inner_indent));

    for i in (0..closure_depth).rev() {
        let indent = format!("{}{}", base, "    ".repeat(i));
        out.push_str(&format!("{}}}\n", indent));
    }

    out.push_str("        }\n");
    out.push_str("    }\n");
}

fn async_callback_param_names(returns: &Option<TypeRef>) -> &'static str {
    match returns {
        None => "context, err",
        Some(TypeRef::Bytes) | Some(TypeRef::BorrowedBytes) | Some(TypeRef::List(_)) => {
            "context, err, result, resultLen"
        }
        Some(TypeRef::Map(_, _)) => "context, err, resultKeys, resultValues, resultLen",
        Some(_) => "context, err, result",
    }
}

fn render_async_resume_result(
    out: &mut String,
    c_prefix: &str,
    returns: &Option<TypeRef>,
    indent: &str,
    module_name: &str,
    func_name: &str,
    ctx: SwiftCtx,
) {
    match returns {
        None => {
            out.push_str(&format!("{}contRef.value.resume(returning: ())\n", indent));
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: -1, message: \"null string\"))\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}let str = String(cString: result)\n", indent));
            out.push_str(&format!(
                "{}weaveffi_free_string(UnsafeMutablePointer(mutating: result))\n",
                indent
            ));
            out.push_str(&format!("{}contRef.value.resume(returning: str)\n", indent));
        }
        Some(TypeRef::Struct(name)) | Some(TypeRef::TypedHandle(name)) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: -1, message: \"null pointer\"))\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: {}(ptr: result))\n",
                indent, name
            ));
        }
        Some(TypeRef::Enum(name)) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: {}(rawValue: result.rawValue)!)\n",
                indent, name
            ));
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                out.push_str(&format!("{}if let result = result {{\n", indent));
                out.push_str(&format!(
                    "{}    let str = String(cString: result)\n",
                    indent
                ));
                out.push_str(&format!(
                    "{}    weaveffi_free_string(UnsafeMutablePointer(mutating: result))\n",
                    indent
                ));
                out.push_str(&format!(
                    "{}    contRef.value.resume(returning: str)\n",
                    indent
                ));
                out.push_str(&format!("{}}} else {{\n", indent));
                out.push_str(&format!(
                    "{}    contRef.value.resume(returning: nil)\n",
                    indent
                ));
                out.push_str(&format!("{}}}\n", indent));
            }
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let name = ctx.ty_name(local_type_name(name));
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result.map {{ {}(ptr: $0) }})\n",
                    indent, name
                ));
            }
            TypeRef::Enum(name) => {
                let name = ctx.ty_name(local_type_name(name));
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result.map {{ {}(rawValue: $0.pointee.rawValue)! }})\n",
                    indent, name
                ));
            }
            _ if is_c_value_type(inner) => {
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result?.pointee)\n",
                    indent
                ));
            }
            _ => {
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result)\n",
                    indent
                ));
            }
        },
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(&format!("{}if let result = result {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: Data(bytes: result, count: Int(resultLen)))\n",
                indent
            ));
            out.push_str(&format!("{}}} else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: Data())\n",
                indent
            ));
            out.push_str(&format!("{}}}\n", indent));
        }
        Some(TypeRef::List(inner)) => {
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: [])\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}let len = Int(resultLen)\n", indent));
            match inner.as_ref() {
                TypeRef::Enum(name) => {
                    let name = ctx.ty_name(local_type_name(name));
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: (0..<len).map {{ {}(rawValue: result[$0].rawValue)! }})\n",
                        indent, name
                    ));
                }
                TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                    let name = ctx.ty_name(local_type_name(name));
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: (0..<len).map {{ {}(ptr: result[$0]!) }})\n",
                        indent, name
                    ));
                }
                _ => {
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: Array(UnsafeBufferPointer(start: result, count: len)))\n",
                        indent
                    ));
                }
            }
        }
        Some(TypeRef::Map(k, v)) => {
            let key_swift = swift_type_ctx(k, ctx);
            let val_swift = swift_type_ctx(v, ctx);
            out.push_str(&format!(
                "{}guard let resultKeys = resultKeys, let resultValues = resultValues else {{\n",
                indent
            ));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: [:])\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}let len = Int(resultLen)\n", indent));
            out.push_str(&format!(
                "{}var dict: [{}: {}] = [:]\n",
                indent, key_swift, val_swift
            ));
            out.push_str(&format!("{}for i in 0..<len {{\n", indent));
            let key_expr = map_element_read(k, "resultKeys[i]", ctx);
            let val_expr = map_element_read(v, "resultValues[i]", ctx);
            out.push_str(&format!(
                "{}    dict[{}] = {}\n",
                indent, key_expr, val_expr
            ));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: dict)\n",
                indent
            ));
        }
        Some(TypeRef::Iterator(inner)) => {
            let pascal_func = func_name.to_upper_camel_case();
            let iter_prefix = format!("{c_prefix}_{module_name}_{pascal_func}Iterator");
            let next_fn = format!("{iter_prefix}_next");
            let destroy_fn = format!("{iter_prefix}_destroy");
            let inner_swift = swift_type_ctx(inner, ctx);

            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: [])\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}var items: [{}] = []\n", indent, inner_swift));
            let elem_c_type = match inner.as_ref() {
                TypeRef::Enum(name) => {
                    format!("{}_{}_{}", c_prefix, module_name, local_type_name(name))
                }
                _ => String::new(),
            };
            render_iter_pull(out, indent, &next_fn, "result", inner, &elem_c_type, ctx);
            out.push_str(&format!("{}{}(result)\n", indent, destroy_fn));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: items)\n",
                indent
            ));
        }
        Some(_) => {
            out.push_str(&format!(
                "{}contRef.value.resume(returning: result)\n",
                indent
            ));
        }
    }
}

fn build_c_call_args(params: &[ParamBinding], c_prefix: &str, module_name: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in params {
        match &p.ty {
            // Strings are a single NUL-terminated `const char*`.
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => args.push(format!("{}_ptr", p.name)),
            // Bytes pass an explicit (ptr, len) pair.
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => args.push(format!("{}.ptr", p.name)),
            TypeRef::Enum(enum_name) => args.push(format!(
                "{c_prefix}_{}_{}({}.rawValue)",
                module_name, enum_name, p.name
            )),
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    args.push(format!("{}?.ptr", p.name))
                }
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => args.push(format!("{}_ptr", p.name)),
                TypeRef::Bytes | TypeRef::BorrowedBytes => {
                    args.push(format!("{}_ptr", p.name));
                    args.push(format!("{}_len", p.name));
                }
                _ => args.push(format!("{}_ptr", p.name)),
            },
            TypeRef::List(_) => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            TypeRef::Map(_, _) => {
                args.push(format!("{}_keys_ptr", p.name));
                args.push(format!("{}_values_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            _ => args.push(p.name.clone()),
        }
    }
    args.join(", ")
}

fn render_direct_call(out: &mut String, f: &FnBinding, call_with_err: &str, ctx: SwiftCtx) {
    match &f.ret {
        None => {
            out.push_str(&format!("        {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: \"null string\") }\n");
            out.push_str("        defer { weaveffi_free_string(rv) }\n");
            out.push_str("        return String(cString: rv)\n");
        }
        Some(TypeRef::Bytes) => {
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!(
                "        let rv = {}\n",
                call_with_err.replace("&err)", "&outLen, &err)")
            ));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { return Data() }\n");
            out.push_str("        defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen) }\n");
            out.push_str("        return Data(bytes: rv, count: outLen)\n");
        }
        Some(TypeRef::Struct(name)) | Some(TypeRef::TypedHandle(name)) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
            out.push_str(&format!("        return {}(ptr: rv)\n", name));
        }
        Some(TypeRef::Enum(name)) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!(
                "        return {}(rawValue: rv.rawValue)!\n",
                name
            ));
        }
        Some(TypeRef::Optional(inner)) => {
            render_optional_return(out, call_with_err, inner, ctx);
        }
        Some(TypeRef::List(inner)) => {
            render_list_return(out, call_with_err, inner, ctx);
        }
        Some(TypeRef::Map(k, v)) => {
            render_map_return(out, call_with_err, k, v, ctx);
        }
        Some(TypeRef::Iterator(_)) => {
            render_iterator_return(out, f, call_with_err, "        ", ctx);
        }
        Some(_) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv\n");
        }
    }
}

fn render_optional_return(out: &mut String, call_with_err: &str, inner: &TypeRef, ctx: SwiftCtx) {
    match inner {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { return nil }\n");
            out.push_str("        defer { weaveffi_free_string(rv) }\n");
            out.push_str("        return String(cString: rv)\n");
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!("        return rv.map {{ {}(ptr: $0) }}\n", name));
        }
        TypeRef::Enum(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!(
                "        return rv.map {{ {}(rawValue: $0.pointee.rawValue)! }}\n",
                name
            ));
        }
        _ if is_c_value_type(inner) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv?.pointee\n");
        }
        _ => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv\n");
        }
    }
}

fn render_list_return(out: &mut String, call_with_err: &str, inner: &TypeRef, ctx: SwiftCtx) {
    out.push_str("        var outLen: Int = 0\n");
    let modified_call = call_with_err.replace("&err)", "&outLen, &err)");
    out.push_str(&format!("        let rv = {}\n", modified_call));
    out.push_str("        try check(&err)\n");
    out.push_str("        guard let rv = rv else { return [] }\n");
    match inner {
        TypeRef::Enum(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!(
                "        return (0..<outLen).map {{ {}(rawValue: rv[$0].rawValue)! }}\n",
                name
            ));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!(
                "        return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                name
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("        return (0..<outLen).map { String(cString: rv[$0]!) }\n");
        }
        _ => {
            out.push_str("        return Array(UnsafeBufferPointer(start: rv, count: outLen))\n");
        }
    }
}

fn render_optional_return_inner(
    out: &mut String,
    call: &str,
    inner: &TypeRef,
    indent: &str,
    ctx: SwiftCtx,
) {
    match inner {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    guard let rv = rv else {{ return nil }}\n",
                indent
            ));
            out.push_str(&format!(
                "{}    defer {{ weaveffi_free_string(rv) }}\n",
                indent
            ));
            out.push_str(&format!("{}    return String(cString: rv)\n", indent));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    return rv.map {{ {}(ptr: $0) }}\n",
                indent, name
            ));
        }
        TypeRef::Enum(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    return rv.map {{ {}(rawValue: $0.pointee.rawValue)! }}\n",
                indent, name
            ));
        }
        _ if is_c_value_type(inner) => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!("{}    return rv?.pointee\n", indent));
        }
        _ => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!("{}    return rv\n", indent));
        }
    }
}

fn render_list_return_inner(
    out: &mut String,
    call: &str,
    inner: &TypeRef,
    indent: &str,
    ctx: SwiftCtx,
) {
    out.push_str(&format!("{}    let rv = {}\n", indent, call));
    out.push_str(&format!("{}    try check(&err)\n", indent));
    out.push_str(&format!(
        "{}    guard let rv = rv else {{ return [] }}\n",
        indent
    ));
    match inner {
        TypeRef::Enum(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!(
                "{}    return (0..<outLen).map {{ {}(rawValue: rv[$0].rawValue)! }}\n",
                indent, name
            ));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!(
                "{}    return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                indent, name
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{}    return (0..<outLen).map {{ String(cString: rv[$0]!) }}\n",
                indent
            ));
        }
        _ => {
            out.push_str(&format!(
                "{}    return Array(UnsafeBufferPointer(start: rv, count: outLen))\n",
                indent
            ));
        }
    }
}

fn swift_c_ptr_element(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "Int8".to_string(),
        TypeRef::I16 => "Int16".to_string(),
        TypeRef::I32 => "Int32".to_string(),
        TypeRef::U8 => "UInt8".to_string(),
        TypeRef::U16 => "UInt16".to_string(),
        TypeRef::U32 => "UInt32".to_string(),
        TypeRef::U64 => "UInt64".to_string(),
        TypeRef::I64 => "Int64".to_string(),
        TypeRef::F32 => "Float".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::Handle => "UInt64".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "UnsafePointer<CChar>?".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "UInt8".to_string(),
        TypeRef::Enum(_) => "Int32".to_string(),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => "OpaquePointer?".to_string(),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) | TypeRef::Iterator(_) => {
            "OpaquePointer?".to_string()
        }
    }
}

fn map_element_read(ty: &TypeRef, expr: &str, ctx: SwiftCtx) -> String {
    match ty {
        TypeRef::StringUtf8 => format!("String(cString: {}!)", expr),
        TypeRef::Enum(name) => {
            format!(
                "{}(rawValue: {}.rawValue)!",
                ctx.ty_name(local_type_name(name)),
                expr
            )
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            format!("{}(ptr: {}!)", ctx.ty_name(local_type_name(name)), expr)
        }
        _ => expr.to_string(),
    }
}

fn render_map_return(
    out: &mut String,
    call_with_err: &str,
    k: &TypeRef,
    v: &TypeRef,
    ctx: SwiftCtx,
) {
    let key_elem = swift_c_ptr_element(k);
    let val_elem = swift_c_ptr_element(v);
    let key_swift = swift_type_ctx(k, ctx);
    let val_swift = swift_type_ctx(v, ctx);

    out.push_str(&format!(
        "        var outKeysPtr: UnsafeMutablePointer<{}>? = nil\n",
        key_elem
    ));
    out.push_str(&format!(
        "        var outValuesPtr: UnsafeMutablePointer<{}>? = nil\n",
        val_elem
    ));
    out.push_str("        var outLen: Int = 0\n");
    let modified_call =
        call_with_err.replace("&err)", "&outKeysPtr, &outValuesPtr, &outLen, &err)");
    out.push_str(&format!("        {}\n", modified_call));
    out.push_str("        try check(&err)\n");
    out.push_str(
        "        guard let outKeys = outKeysPtr, let outValues = outValuesPtr else { return [:] }\n",
    );
    out.push_str(&format!(
        "        var result: [{}: {}] = [:]\n",
        key_swift, val_swift
    ));
    out.push_str("        for i in 0..<outLen {\n");
    let key_expr = map_element_read(k, "outKeys[i]", ctx);
    let val_expr = map_element_read(v, "outValues[i]", ctx);
    out.push_str(&format!(
        "            result[{}] = {}\n",
        key_expr, val_expr
    ));
    out.push_str("        }\n");
    out.push_str("        return result\n");
}

fn render_map_return_inner(
    out: &mut String,
    call: &str,
    k: &TypeRef,
    v: &TypeRef,
    indent: &str,
    ctx: SwiftCtx,
) {
    let key_swift = swift_type_ctx(k, ctx);
    let val_swift = swift_type_ctx(v, ctx);

    out.push_str(&format!("{}    {}\n", indent, call));
    out.push_str(&format!("{}    try check(&err)\n", indent));
    out.push_str(&format!(
        "{}    guard let outKeys = outKeysPtr, let outValues = outValuesPtr else {{ return [:] }}\n",
        indent
    ));
    out.push_str(&format!(
        "{}    var result: [{}: {}] = [:]\n",
        indent, key_swift, val_swift
    ));
    out.push_str(&format!("{}    for i in 0..<outLen {{\n", indent));
    let key_expr = map_element_read(k, "outKeys[i]", ctx);
    let val_expr = map_element_read(v, "outValues[i]", ctx);
    out.push_str(&format!(
        "{}        result[{}] = {}\n",
        indent, key_expr, val_expr
    ));
    out.push_str(&format!("{}    }}\n", indent));
    out.push_str(&format!("{}    return result\n", indent));
}

/// Swift literal initializing the by-value `out_item` slot used while pulling
/// from an iterator whose element lowers to a C value type.
fn swift_scalar_default(ty: &TypeRef) -> String {
    if matches!(ty, TypeRef::Bool) {
        "false".to_string()
    } else {
        "0".to_string()
    }
}

/// Emit the materialization loop for an `iter<T>` `next` symbol. The C ABI is
/// `int32_t next(iter*, T* out_item, …, error* out_err)`, returning 1 when an
/// element was written to `out_item` and 0 at end-of-stream (or on error, with
/// `out_err` set). `iter_ptr` is the already-bound, non-nil handle; elements are
/// appended into a pre-declared `items` array. Declares `iterErr` in the caller's
/// scope so the caller can `check(&iterErr)` after `destroy`. `elem_c_type` is
/// the element's C type and is only consulted for enum elements.
fn render_iter_pull(
    out: &mut String,
    indent: &str,
    next_fn: &str,
    iter_ptr: &str,
    inner: &TypeRef,
    elem_c_type: &str,
    ctx: SwiftCtx,
) {
    let (c_var, default, convert, free): (String, String, String, Option<String>) = match inner {
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => (
            "OpaquePointer?".to_string(),
            "nil".to_string(),
            format!("{}(ptr: iterItem!)", ctx.ty_name(local_type_name(name))),
            None,
        ),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
            "UnsafePointer<CChar>?".to_string(),
            "nil".to_string(),
            "String(cString: iterItem!)".to_string(),
            Some("weaveffi_free_string(UnsafeMutablePointer(mutating: iterItem))".to_string()),
        ),
        TypeRef::Enum(name) => (
            elem_c_type.to_string(),
            format!("{elem_c_type}(0)"),
            format!(
                "{}(rawValue: iterItem.rawValue)!",
                ctx.ty_name(local_type_name(name))
            ),
            None,
        ),
        _ => (
            swift_c_ptr_element(inner),
            swift_scalar_default(inner),
            "iterItem".to_string(),
            None,
        ),
    };
    out.push_str(&format!("{indent}var iterItem: {c_var} = {default}\n"));
    out.push_str(&format!(
        "{indent}var iterErr = weaveffi_error(code: 0, message: nil)\n"
    ));
    out.push_str(&format!(
        "{indent}while {next_fn}({iter_ptr}, &iterItem, &iterErr) != 0 {{\n"
    ));
    out.push_str(&format!("{indent}    items.append({convert})\n"));
    if let Some(free) = free {
        out.push_str(&format!("{indent}    {free}\n"));
    }
    out.push_str(&format!("{indent}}}\n"));
}

fn render_iterator_return(
    out: &mut String,
    f: &FnBinding,
    call_with_err: &str,
    indent: &str,
    ctx: SwiftCtx,
) {
    let it = match &f.shape {
        CallShape::Iterator(it) => it,
        _ => unreachable!("render_iterator_return on non-iterator function"),
    };
    let next_fn = &it.next.symbol;
    let destroy_fn = &it.destroy_symbol;
    let inner = &it.elem;
    let inner_swift = swift_type_ctx(inner, ctx);
    // `out_item` is `params[1]`; render its pointee as the element C type so
    // enum slots get the imported C enum (`{prefix}_{module}_{Name}`).
    let elem_c_type = it
        .next
        .params
        .get(1)
        .map(|p| {
            p.ty.render_c(ctx.c_prefix)
                .trim_end_matches('*')
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    out.push_str(&format!("{indent}let iter = {call_with_err}\n"));
    out.push_str(&format!("{indent}try check(&err)\n"));
    out.push_str(&format!(
        "{indent}guard let iter = iter else {{ return [] }}\n"
    ));
    out.push_str(&format!("{indent}var items: [{inner_swift}] = []\n"));
    render_iter_pull(out, indent, next_fn, "iter", inner, &elem_c_type, ctx);
    out.push_str(&format!("{indent}{destroy_fn}(iter)\n"));
    out.push_str(&format!("{indent}try check(&iterErr)\n"));
    out.push_str(&format!("{indent}return items\n"));
}

fn is_string_elem(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr)
}

/// The staged Swift array a `[T]` list param is read from inside the
/// `withUnsafeBufferPointer` closure. Strings are first copied into a
/// `[UnsafePointer<CChar>?]` (see [`stage_cstring_arrays`]); enums/structs use
/// their pre-mapped raw/pointer arrays; scalars are passed through.
fn list_array_source(inner: &TypeRef, name: &str) -> String {
    match inner {
        TypeRef::Enum(_) => format!("{name}_raw"),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("{name}_ptrs"),
        _ if is_string_elem(inner) => format!("{name}_cstrs"),
        _ => name.to_string(),
    }
}

fn map_array_source(ty: &TypeRef, name: &str, suffix: &str) -> String {
    match ty {
        TypeRef::Enum(_) => format!("{name}_{suffix}Raw"),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("{name}_{suffix}Ptrs"),
        _ if is_string_elem(ty) => format!("{name}_{suffix}Cstrs"),
        _ => format!("{name}_{suffix}"),
    }
}

/// Copy the string elements of a list/map param into heap `[UnsafePointer<CChar>?]`
/// arrays so they can be handed to the C ABI as `const char* const*`. A
/// `defer` frees the copies once the surrounding call returns (the producer
/// copies inputs synchronously). For map params this assumes the `_keys` and
/// `_values` staging arrays already exist.
fn stage_cstring_arrays(out: &mut String, base: &str, p: &ParamBinding) {
    let n = &p.name;
    let emit = |out: &mut String, var: &str, from: &str| {
        let _ = writeln!(
            out,
            "{base}let {var}: [UnsafePointer<CChar>?] = {from}.map {{ UnsafePointer(strdup($0)) }}"
        );
        let _ = writeln!(
            out,
            "{base}defer {{ {var}.forEach {{ if let s = $0 {{ free(UnsafeMutablePointer(mutating: s)) }} }} }}"
        );
    };
    match &p.ty {
        TypeRef::List(inner) if is_string_elem(inner) => {
            emit(out, &format!("{n}_cstrs"), n);
        }
        TypeRef::Map(k, v) => {
            if is_string_elem(k) {
                emit(out, &format!("{n}_keysCstrs"), &format!("{n}_keys"));
            }
            if is_string_elem(v) {
                emit(out, &format!("{n}_valuesCstrs"), &format!("{n}_values"));
            }
        }
        _ => {}
    }
}

/// The prefix for a `withX { ... }` buffer-staging closure. The outermost
/// closure binds `result`; every inner closure `return`s its value so the call
/// result propagates back out through the nesting (closures carrying `let _ptr`
/// setup lines are multi-statement and would otherwise drop it). `try` is added
/// when the innermost body calls `try check`. Void calls emit a bare statement.
fn closure_open(is_first: bool, needs_return: bool, needs_try: bool, ret_type: &str) -> String {
    let t = if needs_try { "try " } else { "" };
    if !needs_return {
        t.to_string()
    } else if is_first {
        format!("let result: {ret_type} = {t}")
    } else {
        format!("return {t}")
    }
}

fn render_buffered_call(
    out: &mut String,
    c_prefix: &str,
    f: &FnBinding,
    params: &[ParamBinding],
    module_name: &str,
    ctx: SwiftCtx,
) {
    for p in params {
        match &p.ty {
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("        let {n}_bytes = Array({n})\n", n = p.name));
            }
            TypeRef::Optional(inner) => {
                if let TypeRef::Enum(enum_name) = inner.as_ref() {
                    out.push_str(&format!(
                        "        let {n}_c: {c_prefix}_{m}_{e}? = {n}.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        n = p.name, m = module_name, e = enum_name
                    ));
                }
            }
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::Enum(enum_name) => {
                    out.push_str(&format!(
                        "        let {n}_raw = {n}.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!(
                        "        let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::Map(k, v) => {
                out.push_str(&format!(
                    "        let {n}_keys = Array({n}.keys)\n",
                    n = p.name
                ));
                out.push_str(&format!(
                    "        let {n}_values = {n}_keys.map {{ {n}[$0]! }}\n",
                    n = p.name
                ));
                match k.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_keysRaw = {n}_keys.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                            n = p.name, m = module_name, e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_keysPtrs = {n}_keys.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
                match v.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_valuesRaw = {n}_values.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                            n = p.name, m = module_name, e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_valuesPtrs = {n}_values.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        stage_cstring_arrays(out, "        ", p);
    }

    let closure_params: Vec<&ParamBinding> =
        params.iter().filter(|p| needs_closure(&p.ty)).collect();

    let is_list_return = matches!(f.ret.as_ref(), Some(TypeRef::List(_)));
    let is_map_return = matches!(f.ret.as_ref(), Some(TypeRef::Map(_, _)));
    if is_list_return || is_map_return {
        out.push_str("        var outLen: Int = 0\n");
    }
    if let Some(TypeRef::Map(k, v)) = &f.ret {
        let key_elem = swift_c_ptr_element(k);
        let val_elem = swift_c_ptr_element(v);
        out.push_str(&format!(
            "        var outKeysPtr: UnsafeMutablePointer<{}>? = nil\n",
            key_elem
        ));
        out.push_str(&format!(
            "        var outValuesPtr: UnsafeMutablePointer<{}>? = nil\n",
            val_elem
        ));
    }

    let handles_return_inside = matches!(
        f.ret.as_ref(),
        Some(TypeRef::StringUtf8)
            | Some(TypeRef::Enum(_))
            | Some(TypeRef::Optional(_))
            | Some(TypeRef::List(_))
            | Some(TypeRef::Map(_, _))
            | Some(TypeRef::Iterator(_))
    );

    let ret_type = match &f.ret {
        Some(TypeRef::Struct(_) | TypeRef::TypedHandle(_)) => "OpaquePointer?".to_string(),
        Some(ty) => swift_type_for(ty),
        None => "Void".to_string(),
    };
    let needs_return = f.ret.is_some();

    let mut closure_depth: usize = 0;
    for p in &closure_params {
        let indent = "        ".to_string() + &"    ".repeat(closure_depth);
        let is_first = closure_depth == 0;
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withCString {{ {}_ptr in\n",
                    indent, open, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner)
                if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
            {
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}withOptionalCString({}) {{ {}_ptr in\n",
                    indent, open, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, open, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress!\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}withOptionalPointer(to: {}) {{ {}_ptr in\n",
                    indent, open, source, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::List(inner) => {
                let source = list_array_source(inner, &p.name);
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, open, source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Map(k, v) => {
                let keys_source = map_array_source(k, &p.name, "keys");
                let values_source = map_array_source(v, &p.name, "values");
                let keys_open =
                    closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                    indent, keys_open, keys_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_keys_ptr = {}_keys_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
                let vind = "        ".to_string() + &"    ".repeat(closure_depth);
                let values_open =
                    closure_open(false, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withUnsafeBufferPointer {{ {}_values_buf in\n",
                    vind, values_open, values_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_values_ptr = {}_values_buf.baseAddress\n",
                    vind, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_values_buf.count\n",
                    vind, p.name, p.name
                ));
                closure_depth += 1;
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = "        ".to_string() + &"    ".repeat(closure_depth);
    let c_sym = &f.c_base;
    let call_args = build_c_call_args(params, c_prefix, module_name);
    let call_with_err = if is_map_return {
        if call_args.is_empty() {
            format!("{}(&outKeysPtr, &outValuesPtr, &outLen, &err)", c_sym)
        } else {
            format!(
                "{}({}, &outKeysPtr, &outValuesPtr, &outLen, &err)",
                c_sym, call_args
            )
        }
    } else if is_list_return {
        if call_args.is_empty() {
            format!("{}(&outLen, &err)", c_sym)
        } else {
            format!("{}({}, &outLen, &err)", c_sym, call_args)
        }
    } else if call_args.is_empty() {
        format!("{}(&err)", c_sym)
    } else {
        format!("{}({}, &err)", c_sym, call_args)
    };

    match &f.ret {
        None => {
            out.push_str(&format!("{}    {}\n", inner_indent, call_with_err));
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("{}    let rv = {}\n", inner_indent, call_with_err));
            out.push_str(&format!("{}    try check(&err)\n", inner_indent));
            out.push_str(&format!("{}    guard let rv = rv else {{ throw WeaveFFIError.error(code: -1, message: \"null string\") }}\n", inner_indent));
            out.push_str(&format!(
                "{}    defer {{ weaveffi_free_string(rv) }}\n",
                inner_indent
            ));
            out.push_str(&format!("{}    return String(cString: rv)\n", inner_indent));
        }
        Some(TypeRef::Enum(name)) => {
            let name = ctx.ty_name(local_type_name(name));
            out.push_str(&format!("{}    let rv = {}\n", inner_indent, call_with_err));
            out.push_str(&format!("{}    try check(&err)\n", inner_indent));
            out.push_str(&format!(
                "{}    return {}(rawValue: rv.rawValue)!\n",
                inner_indent, name
            ));
        }
        Some(TypeRef::Optional(inner)) => {
            render_optional_return_inner(out, &call_with_err, inner, &inner_indent, ctx);
        }
        Some(TypeRef::List(inner)) => {
            render_list_return_inner(out, &call_with_err, inner, &inner_indent, ctx);
        }
        Some(TypeRef::Map(k, v)) => {
            render_map_return_inner(out, &call_with_err, k, v, &inner_indent, ctx);
        }
        Some(TypeRef::Iterator(_)) => {
            let ind = format!("{}    ", inner_indent);
            render_iterator_return(out, f, &call_with_err, &ind, ctx);
        }
        Some(_) => {
            out.push_str(&format!("{}    return {}\n", inner_indent, call_with_err));
        }
    }

    for i in (0..closure_depth).rev() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        out.push_str(&format!("{}}}\n", indent));
    }

    if f.ret.is_none() {
        out.push_str("        try check(&err)\n");
    } else if let Some(TypeRef::Struct(name)) | Some(TypeRef::TypedHandle(name)) = &f.ret {
        let name = ctx.ty_name(local_type_name(name));
        out.push_str("        try check(&err)\n");
        out.push_str("        guard let result = result else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
        out.push_str(&format!("        return {}(ptr: result)\n", name));
    } else if handles_return_inside {
        out.push_str("        return result\n");
    } else {
        out.push_str("        try check(&err)\n");
        out.push_str("        return result\n");
    }
}

/// Like `render_buffered_call`, but calls the constructor symbol `create_sym`
/// and always returns a wrapper pointer. Shared by struct builders
/// (`{c_tag}_create`) and rich-enum variant factories (`{c_tag}_{variant}_new`).
fn render_buffered_struct_create(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    create_sym: &str,
    params: &[ParamBinding],
    struct_class_name: &str,
) {
    for p in params {
        match &p.ty {
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("        let {n}_bytes = Array({n})\n", n = p.name));
            }
            TypeRef::Optional(inner) => {
                if let TypeRef::Enum(enum_name) = inner.as_ref() {
                    out.push_str(&format!(
                        "        let {n}_c: {c_prefix}_{m}_{e}? = {n}.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
            }
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::Enum(enum_name) => {
                    out.push_str(&format!(
                        "        let {n}_raw = {n}.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!(
                        "        let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::Map(k, v) => {
                out.push_str(&format!(
                    "        let {n}_keys = Array({n}.keys)\n",
                    n = p.name
                ));
                out.push_str(&format!(
                    "        let {n}_values = {n}_keys.map {{ {n}[$0]! }}\n",
                    n = p.name
                ));
                match k.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_keysRaw = {n}_keys.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                            n = p.name,
                            m = module_name,
                            e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_keysPtrs = {n}_keys.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
                match v.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_valuesRaw = {n}_values.map {{ {c_prefix}_{m}_{e}($0.rawValue) }}\n",
                            n = p.name,
                            m = module_name,
                            e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_valuesPtrs = {n}_values.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        stage_cstring_arrays(out, "        ", p);
    }

    let closure_params: Vec<&ParamBinding> =
        params.iter().filter(|p| needs_closure(&p.ty)).collect();

    let ret_type = "OpaquePointer?".to_string();
    let needs_return = true;
    let handles_return_inside = false;

    let mut closure_depth: usize = 0;
    for p in &closure_params {
        let indent = "        ".to_string() + &"    ".repeat(closure_depth);
        let is_first = closure_depth == 0;
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withCString {{ {}_ptr in\n",
                    indent, open, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner)
                if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
            {
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}withOptionalCString({}) {{ {}_ptr in\n",
                    indent, open, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, open, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress!\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}withOptionalPointer(to: {}) {{ {}_ptr in\n",
                    indent, open, source, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::List(inner) => {
                let source = list_array_source(inner, &p.name);
                let open = closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, open, source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Map(k, v) => {
                let keys_source = map_array_source(k, &p.name, "keys");
                let values_source = map_array_source(v, &p.name, "values");
                let keys_open =
                    closure_open(is_first, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                    indent, keys_open, keys_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_keys_ptr = {}_keys_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
                let vind = "        ".to_string() + &"    ".repeat(closure_depth);
                let values_open =
                    closure_open(false, needs_return, handles_return_inside, &ret_type);
                out.push_str(&format!(
                    "{}{}{}.withUnsafeBufferPointer {{ {}_values_buf in\n",
                    vind, values_open, values_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_values_ptr = {}_values_buf.baseAddress\n",
                    vind, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_values_buf.count\n",
                    vind, p.name, p.name
                ));
                closure_depth += 1;
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = "        ".to_string() + &"    ".repeat(closure_depth);
    let call_args = build_c_call_args(params, c_prefix, module_name);
    let call_with_err = if call_args.is_empty() {
        format!("{}(&err)", create_sym)
    } else {
        format!("{}({}, &err)", create_sym, call_args)
    };

    out.push_str(&format!("{}    return {}\n", inner_indent, call_with_err));

    for i in (0..closure_depth).rev() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        out.push_str(&format!("{}}}\n", indent));
    }

    out.push_str("        try check(&err)\n");
    out.push_str(
        "        guard let result = result else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n",
    );
    out.push_str(&format!(
        "        return {}(ptr: result)\n",
        struct_class_name
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".to_string(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn empty_module(name: &str) -> Module {
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
    fn package_uses_binary_target_and_bundles_slices() {
        use camino::Utf8Path;
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![empty_module("calc")]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::MacosX64, "/s/darwin-x64/libcalculator.dylib");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &SwiftGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &SwiftConfig::default(),
        )
        .expect("swift supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("swift/lib/darwin-arm64/libcalculator.dylib")));
        let pkg = files
            .iter()
            .find(|f| f.path.as_str().ends_with("swift/Package.swift"))
            .expect("Package.swift present");
        let FileContent::Text(txt) = &pkg.content else {
            panic!("Package.swift is text");
        };
        assert!(
            txt.contains(".binaryTarget(") && txt.contains(".xcframework"),
            "binaryTarget xcframework missing: {txt}"
        );
    }

    #[test]
    fn listeners_generate_register_unregister() {
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
        let swift = render_swift_wrapper(&api, "weaveffi", false, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            swift.contains("final class WvCallbackBox<T>"),
            "callback box must be emitted: {swift}"
        );
        assert!(
            swift.contains(
                "public static func events_register_message_listener(_ callback: @escaping (String) -> Void) -> UInt64"
            ),
            "register wrapper missing: {swift}"
        );
        assert!(
            swift.contains("public static func events_unregister_message_listener(_ id: UInt64)"),
            "unregister wrapper missing: {swift}"
        );
        assert!(
            swift.contains("cb(String(cString: message!))"),
            "trampoline must convert the string arg: {swift}"
        );
        assert!(
            swift.contains("Unmanaged.passRetained(box).toOpaque()"),
            "closure box must be retained through context: {swift}"
        );
        assert!(
            swift.contains(".fromOpaque(ctx).release()"),
            "unregister must release the retained box: {swift}"
        );
    }

    #[test]
    fn swift_type_for_struct_returns_name() {
        assert_eq!(
            swift_type_for(&TypeRef::Struct("Contact".into())),
            "Contact"
        );
    }

    #[test]
    fn swift_type_for_enum_returns_name() {
        assert_eq!(swift_type_for(&TypeRef::Enum("Color".into())), "Color");
    }

    #[test]
    fn swift_type_for_optional_wraps_inner() {
        assert_eq!(
            swift_type_for(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "Int32?"
        );
        assert_eq!(
            swift_type_for(&TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            )))),
            "Contact?"
        );
    }

    #[test]
    fn swift_type_for_list_wraps_inner() {
        assert_eq!(
            swift_type_for(&TypeRef::List(Box::new(TypeRef::I32))),
            "[Int32]"
        );
        assert_eq!(
            swift_type_for(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
            "[Color]"
        );
    }

    #[test]
    fn render_enum_declaration() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![],
            structs: vec![],
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("public enum Color: UInt32 {"),
            "missing enum declaration: {out}"
        );
        assert!(out.contains("case red = 0"), "missing red variant: {out}");
        assert!(
            out.contains("case green = 1"),
            "missing green variant: {out}"
        );
        assert!(out.contains("case blue = 2"), "missing blue variant: {out}");
    }

    #[test]
    fn render_enum_variant_camel_case() {
        let api = make_api(vec![Module {
            name: "status".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Status".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "InProgress".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "AllDone".to_string(),
                        value: 1,
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("case inProgress = 0"),
            "missing camelCase variant: {out}"
        );
        assert!(
            out.contains("case allDone = 1"),
            "missing camelCase variant: {out}"
        );
    }

    #[test]
    fn render_function_with_enum_param_and_return() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "mix".to_string(),
                params: vec![Param {
                    name: "a".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(out.contains("_ a: Color"), "missing enum param type: {out}");
        assert!(
            out.contains("-> Color {"),
            "missing enum return type: {out}"
        );
        assert!(
            out.contains("weaveffi_paint_Color(a.rawValue)"),
            "missing enum-to-C conversion: {out}"
        );
        assert!(
            out.contains("Color(rawValue: rv.rawValue)!"),
            "missing C-to-enum conversion: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_value_param() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ id: Int32?"),
            "missing optional param type: {out}"
        );
        assert!(
            out.contains("withOptionalPointer(to: id)"),
            "missing withOptionalPointer call: {out}"
        );
        assert!(out.contains("id_ptr"), "missing pointer binding: {out}");
    }

    #[test]
    fn render_function_with_optional_struct_param() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "update".to_string(),
                params: vec![Param {
                    name: "person".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into()))),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ person: Contact?"),
            "missing optional struct param: {out}"
        );
        assert!(
            out.contains("person?.ptr"),
            "missing optional struct ptr access: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_value_return() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "lookup".to_string(),
                params: vec![Param {
                    name: "key".to_string(),
                    ty: TypeRef::I32,
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> Int32? {"),
            "missing optional return type: {out}"
        );
        assert!(
            out.contains("rv?.pointee"),
            "missing pointer dereference: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_string_return() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_name".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> String? {"),
            "missing optional string return type: {out}"
        );
        assert!(
            out.contains("guard let rv = rv else { return nil }"),
            "missing nil guard: {out}"
        );
        assert!(
            out.contains("weaveffi_free_string(rv)"),
            "missing free_string: {out}"
        );
    }

    #[test]
    fn render_function_with_list_param() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "process".to_string(),
                params: vec![Param {
                    name: "ids".to_string(),
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
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ ids: [Int32]"),
            "missing list param type: {out}"
        );
        assert!(
            out.contains(".withUnsafeBufferPointer"),
            "missing withUnsafeBufferPointer: {out}"
        );
        assert!(out.contains("ids_ptr"), "missing pointer binding: {out}");
        assert!(out.contains("ids_len"), "missing length binding: {out}");
    }

    #[test]
    fn render_function_with_list_return() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "get_ids".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> [Int32] {"),
            "missing list return type: {out}"
        );
        assert!(
            out.contains("var outLen: Int = 0"),
            "missing outLen declaration: {out}"
        );
        assert!(out.contains("&outLen"), "missing outLen in call: {out}");
        assert!(
            out.contains("UnsafeBufferPointer(start: rv, count: outLen)"),
            "missing buffer-to-array conversion: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_struct_return() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
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
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> Contact? {"),
            "missing optional struct return: {out}"
        );
        assert!(
            out.contains("rv.map { Contact(ptr: $0) }"),
            "missing optional struct wrapping: {out}"
        );
    }

    #[test]
    fn render_with_optional_pointer_helper() {
        let api = make_api(vec![]);
        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("func withOptionalPointer<T, R>"),
            "missing withOptionalPointer helper: {out}"
        );
        assert!(
            out.contains("guard let value = value else { return try body(nil) }"),
            "missing nil guard in helper: {out}"
        );
    }

    #[test]
    fn render_struct_class_with_fields() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![],
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
                        name: "age".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("public class Contact {"),
            "missing class declaration: {out}"
        );
        assert!(
            out.contains("let ptr: OpaquePointer"),
            "missing ptr property: {out}"
        );
        assert!(
            out.contains("init(ptr: OpaquePointer)"),
            "missing init: {out}"
        );
        assert!(
            out.contains("weaveffi_contacts_Contact_destroy(ptr)"),
            "missing destroy in deinit: {out}"
        );
        assert!(
            out.contains("public var name: String {"),
            "missing name getter: {out}"
        );
        assert!(
            out.contains("weaveffi_contacts_Contact_get_name(ptr)"),
            "missing name getter call: {out}"
        );
        assert!(
            out.contains("String(cString: raw)"),
            "missing string conversion: {out}"
        );
        assert!(
            out.contains("weaveffi_free_string(raw)"),
            "missing free_string: {out}"
        );
        assert!(
            out.contains("public var age: Int32 {"),
            "missing age getter: {out}"
        );
        assert!(
            out.contains("weaveffi_contacts_Contact_get_age(ptr)"),
            "missing age getter call: {out}"
        );
    }

    #[test]
    fn swift_builder_generated() {
        let api = Api {
            version: "0.4.0".into(),
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
            package: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_swift_builder");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");
        SwiftGenerator
            .generate(&api, out_dir, &SwiftConfig::default())
            .unwrap();
        let swift = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("WeaveFFI")
                .join("WeaveFFI.swift"),
        )
        .unwrap();
        assert!(
            swift.contains("public class ContactBuilder"),
            "missing builder class: {swift}"
        );
        assert!(
            swift.contains("func withName("),
            "missing withName: {swift}"
        );
        assert!(swift.contains("func withAge("), "missing withAge: {swift}");
        assert!(swift.contains("func build()"), "missing build: {swift}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn swift_custom_prefix_threads_to_user_symbols() {
        let api = make_api(vec![Module {
            name: "demo".to_string(),
            functions: vec![Function {
                name: "paint".to_string(),
                params: vec![Param {
                    name: "c".to_string(),
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
            structs: vec![StructDef {
                name: "Point".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "y".to_string(),
                        ty: TypeRef::I32,
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
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_custom_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");
        let config = SwiftConfig {
            prefix: Some("myffi".to_string()),
            ..Default::default()
        };
        SwiftGenerator.generate(&api, out_dir, &config).unwrap();

        let swift = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("WeaveFFI")
                .join("WeaveFFI.swift"),
        )
        .unwrap();
        let modulemap = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("CWeaveFFI")
                .join("module.modulemap"),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&tmp);

        // User symbols honor the configured ABI prefix: the function symbol,
        // the enum-to-C cast, and the struct getter all carry `myffi_`.
        assert!(
            swift.contains("myffi_demo_paint"),
            "function user symbol should use custom prefix: {swift}"
        );
        assert!(
            swift.contains("myffi_demo_Color("),
            "enum-cast user symbol should use custom prefix: {swift}"
        );
        assert!(
            swift.contains("myffi_demo_Point_get_x"),
            "struct getter user symbol should use custom prefix: {swift}"
        );
        // No user symbol falls back to the hard-coded `weaveffi_` prefix.
        assert!(
            !swift.contains("weaveffi_demo_"),
            "no user symbol should keep the default prefix: {swift}"
        );
        // The system module map points at the prefixed C header.
        assert!(
            modulemap.contains("header \"../../../c/myffi.h\""),
            "module map should reference the prefixed C header: {modulemap}"
        );
        // Runtime ABI helpers stay literal regardless of the prefix.
        assert!(
            swift.contains("weaveffi_error_clear(&err)"),
            "runtime helper must remain literal: {swift}"
        );
    }

    #[test]
    fn render_function_returning_struct() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create".to_string(),
                params: vec![Param {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> Contact {"),
            "missing struct return type: {out}"
        );
        assert!(
            out.contains("Contact(ptr: rv)"),
            "missing struct wrapping: {out}"
        );
    }

    #[test]
    fn render_function_with_struct_param() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "save".to_string(),
                params: vec![Param {
                    name: "contact".to_string(),
                    ty: TypeRef::Struct("Contact".into()),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ contact: Contact"),
            "missing struct param type: {out}"
        );
        assert!(
            out.contains("contact.ptr"),
            "missing .ptr access for struct param: {out}"
        );
    }

    #[test]
    fn render_struct_with_bytes_field() {
        let api = make_api(vec![Module {
            name: "storage".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Blob".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "data".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("public var data: Data {"),
            "missing bytes getter: {out}"
        );
        assert!(
            out.contains("weaveffi_storage_Blob_get_data(ptr, &outLen)"),
            "missing bytes getter with outLen: {out}"
        );
    }

    #[test]
    fn render_struct_with_nested_struct_field() {
        let api = make_api(vec![Module {
            name: "geo".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Line".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "start".to_string(),
                    ty: TypeRef::Struct("Point".into()),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("public var start: Point {"),
            "missing nested struct getter: {out}"
        );
        assert!(
            out.contains("Point(ptr: weaveffi_geo_Line_get_start(ptr)!)"),
            "missing nested struct wrapping: {out}"
        );
    }

    #[test]
    fn render_function_returning_struct_with_buffer_params() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "find_by_name".to_string(),
                params: vec![Param {
                    name: "query".to_string(),
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
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> Contact {"),
            "missing struct return type with buffer params: {out}"
        );
        assert!(
            out.contains("Contact(ptr: result)"),
            "missing struct wrapping after buffered call: {out}"
        );
    }

    #[test]
    fn generate_swift_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get_contact".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
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
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
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

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        SwiftGenerator
            .generate(
                &api,
                out_dir,
                &SwiftConfig {
                    strip_module_prefix: true,
                    ..SwiftConfig::default()
                },
            )
            .unwrap();

        let swift = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("WeaveFFI")
                .join("WeaveFFI.swift"),
        )
        .unwrap();

        assert!(
            swift.contains("public enum Color: UInt32 {"),
            "missing enum declaration: {swift}"
        );
        assert!(swift.contains("case red = 0"), "missing red case: {swift}");
        assert!(
            swift.contains("case green = 1"),
            "missing green case: {swift}"
        );
        assert!(
            swift.contains("case blue = 2"),
            "missing blue case: {swift}"
        );

        assert!(
            swift.contains("public class Contact {"),
            "missing class declaration: {swift}"
        );
        assert!(
            swift.contains("let ptr: OpaquePointer"),
            "missing ptr property: {swift}"
        );
        assert!(
            swift.contains("public var name: String {"),
            "missing name getter: {swift}"
        );
        assert!(
            swift.contains("public var email: String {"),
            "missing email getter: {swift}"
        );
        assert!(
            swift.contains("public var age: Int32 {"),
            "missing age getter: {swift}"
        );

        assert!(
            swift.contains("public static func get_contact(_ id: Int32) throws -> Contact {"),
            "missing function signature: {swift}"
        );
        assert!(
            swift.contains("Contact(ptr: rv)"),
            "missing struct wrapping: {swift}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn swift_type_for_map() {
        assert_eq!(
            swift_type_for(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "[String: Int32]"
        );
        assert_eq!(
            swift_type_for(&TypeRef::Map(
                Box::new(TypeRef::I32),
                Box::new(TypeRef::F64)
            )),
            "[Int32: Double]"
        );
    }

    #[test]
    fn render_function_with_map_param() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "update_scores".to_string(),
                params: vec![Param {
                    name: "scores".to_string(),
                    ty: TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64)),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ scores: [Int32: Double]"),
            "missing map param type: {out}"
        );
        assert!(
            out.contains("scores_keys = Array(scores.keys)"),
            "missing keys extraction: {out}"
        );
        assert!(
            out.contains("scores_values = scores_keys.map { scores[$0]! }"),
            "missing values extraction: {out}"
        );
        assert!(
            out.contains(".withUnsafeBufferPointer"),
            "missing withUnsafeBufferPointer: {out}"
        );
        assert!(
            out.contains("scores_keys_ptr"),
            "missing keys pointer: {out}"
        );
        assert!(
            out.contains("scores_values_ptr"),
            "missing values pointer: {out}"
        );
        assert!(out.contains("scores_len"), "missing length: {out}");
    }

    #[test]
    fn render_function_with_map_return() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_scores".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64))),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("-> [Int32: Double] {"),
            "missing map return type: {out}"
        );
        assert!(out.contains("var outLen: Int = 0"), "missing outLen: {out}");
        assert!(out.contains("outKeysPtr"), "missing keys out-param: {out}");
        assert!(
            out.contains("outValuesPtr"),
            "missing values out-param: {out}"
        );
        assert!(
            out.contains("var result: [Int32: Double] = [:]"),
            "missing dict construction: {out}"
        );
        assert!(
            out.contains("for i in 0..<outLen"),
            "missing iteration: {out}"
        );
    }

    #[test]
    fn swift_struct_optional_field_getter() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "role".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Enum("Role".into()))),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

        assert!(
            out.contains("public var email: String? {"),
            "missing optional string getter: {out}"
        );
        assert!(
            out.contains("guard let p = p else { return nil }"),
            "missing nil guard for optional string: {out}"
        );
        assert!(
            out.contains("defer { weaveffi_free_string(p) }"),
            "missing free_string for optional string: {out}"
        );
        assert!(
            out.contains("return String(cString: p)"),
            "missing cString conversion: {out}"
        );

        assert!(
            out.contains("public var age: Int32? {"),
            "missing optional i32 getter: {out}"
        );
        assert!(
            out.contains("return p?.pointee"),
            "missing pointee for optional value: {out}"
        );

        assert!(
            out.contains("public var role: Role? {"),
            "missing optional enum getter: {out}"
        );
        assert!(
            out.contains("Role(rawValue: $0.pointee.rawValue)!"),
            "missing optional enum conversion: {out}"
        );
    }

    #[test]
    fn swift_custom_module_name() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![
                    Param {
                        name: "a".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "b".to_string(),
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
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let config = SwiftConfig {
            module_name: Some("MyCoolLib".into()),
            ..SwiftConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_custom_module");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        SwiftGenerator.generate(&api, out_dir, &config).unwrap();

        let pkg = std::fs::read_to_string(tmp.join("swift").join("Package.swift")).unwrap();
        assert!(
            pkg.contains("name: \"MyCoolLib\""),
            "Package.swift should use custom module name: {pkg}"
        );
        assert!(
            pkg.contains("\"CMyCoolLib\""),
            "Package.swift should reference CMyCoolLib: {pkg}"
        );
        assert!(
            !pkg.contains("\"WeaveFFI\""),
            "Package.swift should not reference WeaveFFI as a module name: {pkg}"
        );

        let modulemap = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("CMyCoolLib")
                .join("module.modulemap"),
        )
        .unwrap();
        assert!(
            modulemap.contains("module CMyCoolLib"),
            "modulemap should use custom name: {modulemap}"
        );

        let swift_src = tmp
            .join("swift")
            .join("Sources")
            .join("MyCoolLib")
            .join("MyCoolLib.swift");
        assert!(
            swift_src.exists(),
            "Swift source should be at MyCoolLib/MyCoolLib.swift"
        );

        let swift = std::fs::read_to_string(&swift_src).unwrap();
        assert!(
            swift.contains("import CMyCoolLib"),
            "wrapper must import the renamed C module: {swift}"
        );
        assert!(
            !swift.contains("import CWeaveFFI"),
            "wrapper must not import the default C module when renamed: {swift}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn swift_inline_error_types() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
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
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "ContactError".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "ContactNotFound".to_string(),
                        code: 1001,
                        message: "Contact not found".to_string(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "InvalidInput".to_string(),
                        code: 1002,
                        message: "Invalid input provided".to_string(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

        assert!(
            out.contains("public enum WeaveFFIError: Error, LocalizedError {"),
            "missing LocalizedError conformance: {out}"
        );
        assert!(
            out.contains("case contactNotFound"),
            "missing contactNotFound case: {out}"
        );
        assert!(
            out.contains("case invalidInput"),
            "missing invalidInput case: {out}"
        );
        assert!(
            out.contains("public var errorDescription: String?"),
            "missing errorDescription property: {out}"
        );
        assert!(
            out.contains("case .contactNotFound: return \"Contact not found\""),
            "missing contactNotFound description: {out}"
        );
        assert!(
            out.contains("case .invalidInput: return \"Invalid input provided\""),
            "missing invalidInput description: {out}"
        );
        assert!(
            out.contains("public var errorCode: Int32"),
            "missing errorCode property: {out}"
        );
        assert!(
            out.contains("case .contactNotFound: return 1001"),
            "missing contactNotFound code: {out}"
        );
        assert!(
            out.contains("case .invalidInput: return 1002"),
            "missing invalidInput code: {out}"
        );
        assert!(
            out.contains("case 1001: throw WeaveFFIError.contactNotFound"),
            "missing domain-specific throw in check(): {out}"
        );
        assert!(
            out.contains("case 1002: throw WeaveFFIError.invalidInput"),
            "missing domain-specific throw in check(): {out}"
        );
        assert!(
            out.contains("default: throw WeaveFFIError.error(code: code, message: message)"),
            "missing fallback throw in check(): {out}"
        );
    }

    #[test]
    fn swift_struct_list_field_getter() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Order".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "item_ids".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::Enum("Tag".into()))),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

        assert!(
            out.contains("public var item_ids: [Int32] {"),
            "missing list i32 getter: {out}"
        );
        assert!(
            out.contains("weaveffi_store_Order_get_item_ids(ptr, &outLen)"),
            "missing list getter call with outLen: {out}"
        );
        assert!(
            out.contains("guard let rv = rv else { return [] }"),
            "missing empty-array guard: {out}"
        );
        assert!(
            out.contains("UnsafeBufferPointer(start: rv, count: outLen)"),
            "missing buffer-to-array conversion: {out}"
        );

        assert!(
            out.contains("public var tags: [Tag] {"),
            "missing list enum getter: {out}"
        );
        assert!(
            out.contains("Tag(rawValue: rv[$0].rawValue)!"),
            "missing list enum conversion: {out}"
        );
    }

    #[test]
    fn swift_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create_contact".to_string(),
                params: vec![Param {
                    name: "name".to_string(),
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

        let config = SwiftConfig {
            strip_module_prefix: true,
            ..SwiftConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        SwiftGenerator.generate(&api, out_dir, &config).unwrap();

        let swift =
            std::fs::read_to_string(tmp.join("swift/Sources/WeaveFFI/WeaveFFI.swift")).unwrap();

        assert!(
            swift.contains("func create_contact("),
            "stripped name should be create_contact: {swift}"
        );
        assert!(
            !swift.contains("func contacts_create_contact("),
            "should not contain module-prefixed name: {swift}"
        );
        assert!(
            swift.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {swift}"
        );

        let no_strip_config = SwiftConfig {
            strip_module_prefix: false,
            ..SwiftConfig::default()
        };
        let tmp2 = std::env::temp_dir().join("weaveffi_test_swift_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        SwiftGenerator
            .generate(&api, out_dir2, &no_strip_config)
            .unwrap();

        let swift2 =
            std::fs::read_to_string(tmp2.join("swift/Sources/WeaveFFI/WeaveFFI.swift")).unwrap();

        assert!(
            swift2.contains("func contacts_create_contact("),
            "default should use module-prefixed name: {swift2}"
        );
        assert!(
            swift2.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {swift2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn swift_deeply_nested_optional() {
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
        let swift = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            swift.contains("[Contact?]?"),
            "should contain deeply nested optional type: {swift}"
        );
    }

    #[test]
    fn swift_map_of_lists() {
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
        let swift = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            swift.contains("[String: [Int32]]"),
            "should contain map of lists type: {swift}"
        );
    }

    #[test]
    fn swift_enum_keyed_map() {
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
        let swift = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            swift.contains("[Color: Contact]"),
            "should contain enum-keyed map type: {swift}"
        );
    }

    #[test]
    fn swift_type_for_borrowed_str() {
        assert_eq!(swift_type_for(&TypeRef::BorrowedStr), "String");
    }

    #[test]
    fn swift_type_for_borrowed_bytes() {
        assert_eq!(swift_type_for(&TypeRef::BorrowedBytes), "Data");
    }

    #[test]
    fn swift_function_with_borrowed_str_param() {
        let api = make_api(vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "write".to_string(),
                params: vec![Param {
                    name: "msg".to_string(),
                    ty: TypeRef::BorrowedStr,
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ msg: String"),
            "BorrowedStr param should use String type: {out}"
        );
        assert!(
            out.contains("weaveffi_io_write"),
            "should call the C function: {out}"
        );
    }

    #[test]
    fn swift_function_with_borrowed_bytes_param() {
        let api = make_api(vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "upload".to_string(),
                params: vec![Param {
                    name: "data".to_string(),
                    ty: TypeRef::BorrowedBytes,
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("_ data: Data"),
            "BorrowedBytes param should use Data type: {out}"
        );
        assert!(
            out.contains("weaveffi_io_upload"),
            "should call the C function: {out}"
        );
    }

    #[test]
    fn swift_typed_handle_type() {
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
        let swift = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            swift.contains("_ contact: Contact"),
            "TypedHandle should use class type not UInt64: {swift}"
        );
        assert!(
            swift.contains("contact.ptr"),
            "TypedHandle should extract .ptr: {swift}"
        );
    }

    #[test]
    fn swift_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
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
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

        assert!(
            !out.contains("weaveffi_free_string(name"),
            "borrowed string param must not be freed by the wrapper: {out}"
        );

        let fn_start = out
            .find("public static func find_contact")
            .expect("find_contact wrapper");
        let fn_body = &out[fn_start..];
        let check_pos = fn_body
            .find("try check(&err)")
            .expect("try check in find_contact");
        let contact_ptr_pos = fn_body
            .find("Contact(ptr:")
            .expect("Contact(ptr: in find_contact");
        assert!(
            check_pos < contact_ptr_pos,
            "error must be checked before wrapping the struct return: {out}"
        );

        assert!(
            out.contains("deinit") && out.contains("weaveffi_contacts_Contact_destroy(ptr)"),
            "struct return type should use a class with destroy in deinit: {out}"
        );
    }

    #[test]
    fn swift_null_check_on_optional_return() {
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
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("rv.map { Contact(ptr: $0) }"),
            "optional struct return should map null before wrapping: {out}"
        );
    }

    #[test]
    fn swift_async_function_signature() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("async throws"),
            "missing async throws in signature: {out}"
        );
        assert!(
            out.contains("public static func run(_ id: Int32) async throws -> Int32"),
            "missing complete async function signature: {out}"
        );
    }

    #[test]
    fn swift_async_uses_continuation() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("withCheckedThrowingContinuation"),
            "missing withCheckedThrowingContinuation: {out}"
        );
        assert!(
            out.contains("ContinuationRef"),
            "missing ContinuationRef usage: {out}"
        );
        assert!(
            out.contains("Unmanaged"),
            "missing Unmanaged for context bridging: {out}"
        );
        assert!(
            out.contains("weaveffi_tasks_run_async"),
            "missing async C function call: {out}"
        );
    }

    /// `Unmanaged.passRetained(...)` (the +1 retain that pins the
    /// continuation across the C boundary) must be matched by exactly one
    /// `Unmanaged.fromOpaque(...).takeRetainedValue()` in the C callback so
    /// the continuation is released when the future resolves.
    #[test]
    fn swift_async_pins_callback_for_lifetime() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        let pin_count = out.matches("Unmanaged.passRetained").count();
        let unpin_count = out.matches("takeRetainedValue()").count();
        assert_eq!(
            pin_count, 1,
            "expected exactly one Unmanaged.passRetained, found {pin_count}: {out}"
        );
        assert_eq!(
            unpin_count, 1,
            "expected exactly one takeRetainedValue, found {unpin_count}: {out}"
        );
    }

    #[test]
    fn swift_cross_module_struct() {
        let api = make_api(vec![
            Module {
                name: "types".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Name".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "value".to_string(),
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
                name: "ops".to_string(),
                functions: vec![Function {
                    name: "get_name".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("types.Name".to_string())),
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

        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

        assert!(
            out.contains("-> Name"),
            "cross-module return type should use local name 'Name': {out}"
        );
        assert!(
            out.contains("Name(ptr:"),
            "cross-module struct constructor should use local name 'Name': {out}"
        );
        assert!(
            !out.contains("types.Name"),
            "dot-qualified name should not appear in generated Swift code: {out}"
        );
    }

    #[test]
    fn swift_nested_module_output() {
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
        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("public enum Parent {"),
            "top-level module enum missing: {out}"
        );
        assert!(
            out.contains("public enum Child {"),
            "nested module enum missing: {out}"
        );
        assert!(
            out.contains("weaveffi_parent_outer_fn"),
            "parent C ABI call missing: {out}"
        );
        assert!(
            out.contains("weaveffi_parent_child_inner_fn"),
            "nested child C ABI call missing: {out}"
        );
    }

    #[test]
    fn swift_type_for_iterator() {
        assert_eq!(
            swift_type_for(&TypeRef::Iterator(Box::new(TypeRef::I32))),
            "[Int32]"
        );
        assert_eq!(
            swift_type_for(&TypeRef::Iterator(Box::new(TypeRef::Struct(
                "Contact".into()
            )))),
            "[Contact]"
        );
    }

    #[test]
    fn swift_iterator_return_generates_consumption_code() {
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
        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("ListItemsIterator"),
            "should reference iterator type: {out}"
        );
        assert!(
            out.contains("_next"),
            "should call _next to consume iterator: {out}"
        );
        assert!(
            out.contains("_destroy"),
            "should call _destroy to clean up iterator: {out}"
        );
    }

    #[test]
    fn deprecated_function_generates_annotation() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add_old".to_string(),
                params: vec![
                    Param {
                        name: "a".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "b".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: Some("Use addV2 instead".to_string()),
                since: Some("0.1.0".to_string()),
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let out = render_swift_wrapper(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
        assert!(
            out.contains("@available(*, deprecated, message: \"Use addV2 instead\")"),
            "missing deprecation annotation: {out}"
        );
        assert!(
            out.contains("func add_old("),
            "missing function declaration: {out}"
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
                    fields: vec![],
                }],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "DocsErrors".into(),
                codes: vec![ErrorCode {
                    name: "not_found".into(),
                    code: 1,
                    message: "Not found".into(),
                    doc: Some("Raised when missing".into()),
                }],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn swift_emits_doc_on_function() {
        let out = render_swift_wrapper(
            &doc_api(),
            "weaveffi",
            true,
            "weaveffi.yml",
            "WeaveFFI.swift",
        );
        assert!(out.contains("/// Performs a thing."), "{out}");
    }

    #[test]
    fn swift_emits_doc_on_struct() {
        let out = render_swift_wrapper(
            &doc_api(),
            "weaveffi",
            true,
            "weaveffi.yml",
            "WeaveFFI.swift",
        );
        assert!(out.contains("/// An item we track."), "{out}");
    }

    #[test]
    fn swift_emits_doc_on_enum_variant() {
        let out = render_swift_wrapper(
            &doc_api(),
            "weaveffi",
            true,
            "weaveffi.yml",
            "WeaveFFI.swift",
        );
        assert!(out.contains("/// Kind of item."), "{out}");
        assert!(out.contains("/// A small one"), "{out}");
    }

    #[test]
    fn swift_emits_doc_on_field() {
        let out = render_swift_wrapper(
            &doc_api(),
            "weaveffi",
            true,
            "weaveffi.yml",
            "WeaveFFI.swift",
        );
        assert!(out.contains("/// Stable id"), "{out}");
    }

    #[test]
    fn swift_emits_doc_on_param() {
        let out = render_swift_wrapper(
            &doc_api(),
            "weaveffi",
            true,
            "weaveffi.yml",
            "WeaveFFI.swift",
        );
        assert!(out.contains("/// - Parameter x: the input value"), "{out}");
    }

    /// The `shapes` sample: a rich (algebraic) enum `Shape` (a unit variant, an
    /// f64 payload, two f32 payloads, and a string+u8 payload), a plain C-style
    /// enum `Channel`, and the free functions that take/return `Shape` (lowered
    /// to `TypeRef::Struct`) plus the numerics smoke `sum_bytes`.
    fn shapes_api() -> Api {
        use weaveffi_ir::ir::StructField;
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
    fn rich_enum_emits_opaque_wrapper_class() {
        let out = render_swift_wrapper(
            &shapes_api(),
            "weaveffi",
            false,
            "shapes.yml",
            "Shapes.swift",
        );
        // Opaque-object wrapper mirroring a struct: owns the handle, frees it in
        // deinit via the enum's _destroy. It must NOT be a plain Swift enum.
        assert!(
            out.contains("public class Shape {"),
            "missing wrapper class: {out}"
        );
        assert!(
            out.contains("let ptr: OpaquePointer"),
            "missing ptr property: {out}"
        );
        assert!(
            out.contains("init(ptr: OpaquePointer)"),
            "missing init: {out}"
        );
        assert!(
            out.contains("deinit {\n        weaveffi_shapes_Shape_destroy(ptr)"),
            "missing destroy in deinit: {out}"
        );
        // A plain enum would be `public enum Shape: <raw> {`; the namespace
        // `public enum Shapes {` must not trip this check.
        assert!(
            !out.contains("public enum Shape:"),
            "rich enum must not be emitted as a plain enum: {out}"
        );
        // The sibling plain C-style enum is still a Swift enum.
        assert!(
            out.contains("public enum Channel: UInt32 {"),
            "plain enum regressed: {out}"
        );
    }

    #[test]
    fn rich_enum_emits_tag_enum_and_reader() {
        let out = render_swift_wrapper(
            &shapes_api(),
            "weaveffi",
            false,
            "shapes.yml",
            "Shapes.swift",
        );
        assert!(
            out.contains("public enum Tag: Int32 {"),
            "missing Tag enum: {out}"
        );
        assert!(out.contains("case empty = 0"), "missing empty tag: {out}");
        assert!(out.contains("case circle = 1"), "missing circle tag: {out}");
        assert!(
            out.contains("case rectangle = 2"),
            "missing rectangle tag: {out}"
        );
        assert!(
            out.contains("case labeled = 3"),
            "missing labeled tag: {out}"
        );
        assert!(
            out.contains("public var tag: Tag {"),
            "missing tag reader: {out}"
        );
        assert!(
            out.contains("return Tag(rawValue: weaveffi_shapes_Shape_tag(ptr))!"),
            "missing tag getter call: {out}"
        );
    }

    #[test]
    fn rich_enum_emits_throwing_variant_factories() {
        let out = render_swift_wrapper(
            &shapes_api(),
            "weaveffi",
            false,
            "shapes.yml",
            "Shapes.swift",
        );
        // Unit variant: only out_err.
        assert!(
            out.contains("public static func empty() throws -> Shape {"),
            "missing empty factory: {out}"
        );
        assert!(
            out.contains("let ptr = weaveffi_shapes_Shape_Empty_new(&err)"),
            "missing empty constructor call: {out}"
        );
        // f64 payload.
        assert!(
            out.contains("public static func circle(_ radius: Double) throws -> Shape {"),
            "missing circle factory: {out}"
        );
        assert!(
            out.contains("let ptr = weaveffi_shapes_Shape_Circle_new(radius, &err)"),
            "missing circle constructor call: {out}"
        );
        // Two f32 payloads.
        assert!(
            out.contains(
                "public static func rectangle(_ width: Float, _ height: Float) throws -> Shape {"
            ),
            "missing rectangle factory: {out}"
        );
        assert!(
            out.contains("let ptr = weaveffi_shapes_Shape_Rectangle_new(width, height, &err)"),
            "missing rectangle constructor call: {out}"
        );
        // string + u8 payload: the string threads through `withCString`.
        assert!(
            out.contains(
                "public static func labeled(_ label: String, _ count: UInt8) throws -> Shape {"
            ),
            "missing labeled factory: {out}"
        );
        assert!(
            out.contains("label.withCString { label_ptr in"),
            "missing string staging for labeled: {out}"
        );
        assert!(
            out.contains("weaveffi_shapes_Shape_Labeled_new(label_ptr, count, &err)"),
            "missing labeled constructor call: {out}"
        );
        // Factories throw on a non-zero error code.
        assert!(
            out.contains("try check(&err)"),
            "factory must check err: {out}"
        );
    }

    #[test]
    fn rich_enum_emits_per_variant_getters() {
        let out = render_swift_wrapper(
            &shapes_api(),
            "weaveffi",
            false,
            "shapes.yml",
            "Shapes.swift",
        );
        // Numeric getters, namespaced by variant to avoid collisions.
        assert!(
            out.contains("public var circleRadius: Double {"),
            "missing circleRadius: {out}"
        );
        assert!(
            out.contains("return weaveffi_shapes_Shape_Circle_get_radius(ptr)"),
            "missing circleRadius call: {out}"
        );
        assert!(
            out.contains("public var rectangleWidth: Float {"),
            "missing rectangleWidth: {out}"
        );
        assert!(
            out.contains("public var rectangleHeight: Float {"),
            "missing rectangleHeight: {out}"
        );
        assert!(
            out.contains("public var labeledCount: UInt8 {"),
            "missing labeledCount: {out}"
        );
        // String getter frees the C string (reuses struct-field marshalling).
        assert!(
            out.contains("public var labeledLabel: String {"),
            "missing labeledLabel: {out}"
        );
        assert!(
            out.contains("let raw = weaveffi_shapes_Shape_Labeled_get_label(ptr)"),
            "missing labeledLabel call: {out}"
        );
        assert!(
            out.contains("weaveffi_free_string(raw)"),
            "labeledLabel must free the C string: {out}"
        );
    }

    #[test]
    fn rich_enum_functions_marshal_the_handle() {
        let out = render_swift_wrapper(
            &shapes_api(),
            "weaveffi",
            false,
            "shapes.yml",
            "Shapes.swift",
        );
        // describe(Shape) -> String: passes the opaque pointer, frees the result.
        assert!(
            out.contains("public static func shapes_describe(_ shape: Shape) throws -> String {"),
            "missing describe signature: {out}"
        );
        assert!(
            out.contains("weaveffi_shapes_describe(shape.ptr, &err)"),
            "describe must pass shape.ptr: {out}"
        );
        // scale(Shape, f64) -> Shape: rich enum in and out (wrapped via init).
        assert!(
            out.contains(
                "public static func shapes_scale(_ shape: Shape, _ factor: Double) throws -> Shape {"
            ),
            "missing scale signature: {out}"
        );
        assert!(
            out.contains("weaveffi_shapes_scale(shape.ptr, factor, &err)"),
            "scale must pass shape.ptr + factor: {out}"
        );
        assert!(
            out.contains("return Shape(ptr: rv)"),
            "scale must wrap the returned handle: {out}"
        );
        // sum_bytes([u8]) -> u64: numerics smoke.
        assert!(
            out.contains(
                "public static func shapes_sum_bytes(_ values: [UInt8]) throws -> UInt64 {"
            ),
            "missing sum_bytes signature: {out}"
        );
    }
}
