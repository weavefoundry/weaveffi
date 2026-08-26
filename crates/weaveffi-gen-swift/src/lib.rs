//! Swift binding generator for WeaveFFI.
//!
//! Emits a SwiftPM package containing a thin Swift wrapper over the C ABI,
//! including module map, `Package.swift`, and Swift `async/await` shims for
//! functions marked `async: true`. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps cross the C ABI as value
//! buffers (one `const uint8_t*` + `size_t` pair in the WeaveFFI wire format).
//! The generated wrapper ships a small private writer/reader pair
//! (`WvWriter`/`WvReader`) plus one pack and one unpack routine per record and
//! rich enum, so records surface as plain Swift structs and rich enums as
//! native Swift enums with associated values.
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
use weaveffi_core::abi::lower::split_qualified;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, walk_modules, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, ErrorBinding, FieldBinding, FnBinding, InterfaceBinding,
    IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::plan::{ElemFree, ErrorStrategy};
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, Module, TypeRef};

/// Per-target configuration for [`SwiftGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwiftConfig {
    /// SwiftPM module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// function names, so `enum Kv` exposes `openStore` rather than
    /// `kvOpenStore`. Set to `false` to restore the module-prefixed spelling.
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

impl Default for SwiftConfig {
    fn default() -> Self {
        Self {
            module_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
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
/// declarations, codec routines, async wrappers); pre-allocating from this
/// estimate reduces `String` re-allocations as the wrapper grows past 64 KB.
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
        model: &BindingModel,
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
                    model,
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
        model: &BindingModel,
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
            model,
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
        // Handles, plain and typed alike, are opaque `u64` resource tokens in
        // the wire format; Swift surfaces both as `UInt64` and converts to
        // the typed C pointer at the direct ABI boundary.
        TypeRef::Handle | TypeRef::TypedHandle(_) => "UInt64".to_string(),
        TypeRef::Enum(name)
        | TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Interface(name) => local_type_name(name).to_string(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Optional(inner) => format!("{}?", swift_type_for(inner)),
        TypeRef::List(inner) => format!("[{}]", swift_type_for(inner)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_for(k), swift_type_for(v)),
        // An iterator return renders as its per-function sequence class (see
        // `render_swift_iterator_class`), never through this generic mapping.
        TypeRef::Iterator(_) => unreachable!("iterator type is only valid as a function return"),
    }
}

/// Context threaded into the function/return renderers so they can emit the
/// fully-prefixed C symbols (for iterators), disambiguate wrapper types that
/// collide with a module namespace, and look up the raw type of a C-style
/// enum inside buffer codecs.
#[derive(Clone, Copy)]
struct SwiftCtx<'a> {
    /// C ABI symbol prefix (e.g. `weaveffi`).
    c_prefix: &'a str,
    /// SwiftPM module name (e.g. `Kvstore`).
    swift_module: &'a str,
    /// Every module name in the API, PascalCased, i.e. the set of namespace
    /// `enum` names that wrapper-type references can be shadowed by.
    module_names: &'a HashSet<String>,
    /// Raw-value Swift type (`"Int32"` or `"UInt32"`) of every C-style enum
    /// in the API, keyed by its bare type name.
    enum_raws: &'a HashMap<String, &'static str>,
}

impl SwiftCtx<'_> {
    /// Qualify a top-level wrapper type name with the Swift module when its
    /// name collides with a namespace `enum`. Inside `enum Kv { enum Stats { … } }`
    /// the bare name `Stats` resolves to the namespace, not the top-level
    /// type; `Kvstore.Stats` forces the type. Module-qualifying is valid from
    /// any scope, so we apply it whenever the name collides.
    fn ty_name(&self, local: &str) -> String {
        if self.module_names.contains(local) {
            format!("{}.{}", self.swift_module, local)
        } else {
            local.to_string()
        }
    }

    /// The raw-value Swift type of the C-style enum named `local`.
    fn enum_raw(&self, local: &str) -> &'static str {
        self.enum_raws.get(local).copied().unwrap_or("UInt32")
    }
}

/// How a wrapper body reports a non-zero `weaveffi_error` slot.
///
/// A callable with `throws == true` maps codes through the declaring module's
/// typed checker (`checkKv`) and surfaces marshalling failures as thrown
/// [`ERROR_BRAND`] values; a callable with `throws == false` has a plain
/// signature and traps (`fatalError`) instead, since a reported error can only
/// be a producer panic or an argument-marshalling failure.
#[derive(Clone, Copy)]
struct ErrCtx<'a> {
    /// `true` when the wrapper is `throws` and surfaces typed errors.
    throws: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `checkKv` and
    /// `mapKv`); `None` falls back to the generic `check` helper.
    domain: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// Build the error context for `f` from its [`ErrorStrategy`]:
    /// [`ErrorStrategy::Throws`] surfaces typed errors, [`ErrorStrategy::Trap`]
    /// traps on the (panic-only) error path.
    fn for_fn(f: &FnBinding, domain: Option<&'a str>) -> Self {
        Self {
            throws: f.error_strategy() == ErrorStrategy::Throws,
            domain,
        }
    }

    /// The statement checking the error slot named `slot`.
    fn check_stmt(&self, slot: &str) -> String {
        if !self.throws {
            return format!("trap(&{slot})");
        }
        match self.domain {
            Some(stem) => format!("try check{stem}(&{slot})"),
            None => format!("try check(&{slot})"),
        }
    }

    /// The statement reporting a marshalling failure (`code`, `msg` are
    /// literals): a thrown [`ERROR_BRAND`] for a throwing wrapper, a trap
    /// otherwise.
    fn fail_stmt(&self, code: i32, msg: &str) -> String {
        if self.throws {
            format!("throw {ERROR_BRAND}.error(code: {code}, message: \"{msg}\")")
        } else {
            format!("fatalError(\"{code}: {msg}\")")
        }
    }

    /// A `guard let {name} = {name} else {{ ... }}` line reporting a
    /// marshalling failure through [`Self::fail_stmt`].
    fn guard_stmt(&self, name: &str, code: i32, msg: &str) -> String {
        format!(
            "guard let {name} = {name} else {{ {} }}",
            self.fail_stmt(code, msg)
        )
    }

    /// The statements an async completion callback runs (after copying the
    /// runtime `code`/`msg` locals) when the error slot reports: copy the
    /// payload and resume throwing the mapped domain error, resume with the
    /// generic brand error, or trap.
    fn async_err_lines(&self) -> Vec<String> {
        if !self.throws {
            return vec!["fatalError(\"\\(code): \\(msg)\")".to_string()];
        }
        match self.domain {
            Some(stem) => vec![
                "let payload: [UInt8]? = err.pointee.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.pointee.payload_len)) }".to_string(),
                format!("contRef.value.resume(throwing: map{stem}(code: code, message: msg, payload: payload))"),
            ],
            None => vec![format!(
                "contRef.value.resume(throwing: {ERROR_BRAND}.error(code: code, message: msg))"
            )],
        }
    }

    /// The statement an async completion callback uses for a marshalling
    /// failure with literal `code`/`msg`.
    fn async_fail_stmt(&self, code: i32, msg: &str) -> String {
        if self.throws {
            format!(
                "contRef.value.resume(throwing: {ERROR_BRAND}.error(code: {code}, message: \"{msg}\"))"
            )
        } else {
            format!("fatalError(\"{code}: {msg}\")")
        }
    }

    /// The Swift error type parameter of the continuation: `Error` for a
    /// throwing wrapper, `Never` for a plain one.
    fn continuation_err_ty(&self) -> &'static str {
        if self.throws {
            "Error"
        } else {
            "Never"
        }
    }
}

/// The PascalCase helper stem of the domain in effect for `module`, naming the
/// per-domain `check{Stem}`/`map{Stem}` helpers (derived from the *declaring*
/// module's path, so inheriting submodules reference the ancestor's helper).
fn domain_stem(module: &ModuleBinding) -> Option<String> {
    module
        .error
        .as_ref()
        .map(|e| e.owner_path.to_upper_camel_case())
}

/// Like [`swift_type_for`] but disambiguates wrapper-type names that collide
/// with a module namespace (see [`SwiftCtx::ty_name`]).
fn swift_type_ctx(t: &TypeRef, ctx: SwiftCtx) -> String {
    match t {
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Enum(name)
        | TypeRef::Interface(name) => ctx.ty_name(local_type_name(name)),
        TypeRef::Optional(inner) => format!("{}?", swift_type_ctx(inner, ctx)),
        TypeRef::List(inner) => format!("[{}]", swift_type_ctx(inner, ctx)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_ctx(k, ctx), swift_type_ctx(v, ctx)),
        _ => swift_type_for(t),
    }
}

/// How one parameter is staged before the C call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Staging {
    /// Passed directly in the argument list (scalars, enums, handles,
    /// interface pointers).
    Direct,
    /// A NUL-terminated `const char*` staged via `withCString`.
    CString,
    /// A `(ptr, len)` pair staged via `withUnsafeBufferPointer` over a
    /// `[UInt8]` copy of the `Data` value.
    Bytes,
    /// A buffered value: packed into a `WvWriter`, then staged via
    /// `withUnsafeBufferPointer` as a `(ptr, len)` pair.
    Buffered,
}

fn staging_for(ty: &TypeRef) -> Staging {
    if abi::is_buffered(ty) {
        return Staging::Buffered;
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => Staging::CString,
        TypeRef::Bytes | TypeRef::BorrowedBytes => Staging::Bytes,
        _ => Staging::Direct,
    }
}

/// The private Swift buffer runtime implementing the WeaveFFI value-buffer
/// wire format: little-endian, packed, no alignment. `WvWriter` serializes,
/// `WvReader` deserializes and traps (via `wvDecodeFailure`) on malformed
/// input, which the spec routes through the same channel as a producer panic.
const BUFFER_RUNTIME: &str = r#"/// Serializes values into the WeaveFFI value-buffer wire format
/// (little-endian, packed, no alignment).
struct WvWriter {
    var bytes: [UInt8] = []

    mutating func writeBool(_ v: Bool) { bytes.append(v ? 1 : 0) }
    mutating func writeInt8(_ v: Int8) { bytes.append(UInt8(bitPattern: v)) }
    mutating func writeUInt8(_ v: UInt8) { bytes.append(v) }
    mutating func writeUInt16(_ v: UInt16) {
        bytes.append(UInt8(truncatingIfNeeded: v))
        bytes.append(UInt8(truncatingIfNeeded: v >> 8))
    }
    mutating func writeInt16(_ v: Int16) { writeUInt16(UInt16(bitPattern: v)) }
    mutating func writeUInt32(_ v: UInt32) {
        bytes.append(UInt8(truncatingIfNeeded: v))
        bytes.append(UInt8(truncatingIfNeeded: v >> 8))
        bytes.append(UInt8(truncatingIfNeeded: v >> 16))
        bytes.append(UInt8(truncatingIfNeeded: v >> 24))
    }
    mutating func writeInt32(_ v: Int32) { writeUInt32(UInt32(bitPattern: v)) }
    mutating func writeUInt64(_ v: UInt64) {
        writeUInt32(UInt32(truncatingIfNeeded: v))
        writeUInt32(UInt32(truncatingIfNeeded: v >> 32))
    }
    mutating func writeInt64(_ v: Int64) { writeUInt64(UInt64(bitPattern: v)) }
    mutating func writeFloat(_ v: Float) { writeUInt32(v.bitPattern) }
    mutating func writeDouble(_ v: Double) { writeUInt64(v.bitPattern) }
    mutating func writeLen(_ n: Int) {
        precondition(n >= 0 && n <= Int(UInt32.max), "WeaveFFI buffer length exceeds UInt32.max")
        writeUInt32(UInt32(n))
    }
    mutating func writeString(_ v: String) {
        let utf8 = Array(v.utf8)
        writeLen(utf8.count)
        bytes.append(contentsOf: utf8)
    }
    mutating func writeBytes(_ v: Data) {
        writeLen(v.count)
        bytes.append(contentsOf: v)
    }
    mutating func writeOptionFlag(_ present: Bool) { bytes.append(present ? 1 : 0) }
}

/// Traps on a malformed value buffer. Per the wire-format spec, consumers
/// surface decode failures through the same channel as a producer panic.
func wvDecodeFailure(_ context: String) -> Never {
    fatalError("malformed WeaveFFI value buffer: \(context)")
}

/// Deserializes values from the WeaveFFI value-buffer wire format, rejecting
/// truncated buffers, invalid flag bytes, oversized length prefixes, and
/// trailing bytes.
struct WvReader {
    let bytes: [UInt8]
    var pos: Int = 0

    init(bytes: [UInt8]) { self.bytes = bytes }

    var remaining: Int { bytes.count - pos }

    mutating func take(_ n: Int, _ context: String) -> ArraySlice<UInt8> {
        guard remaining >= n else { wvDecodeFailure(context) }
        defer { pos += n }
        return bytes[pos..<(pos + n)]
    }

    mutating func readBool() -> Bool {
        switch take(1, "bool").first! {
        case 0: return false
        case 1: return true
        default: wvDecodeFailure("bool byte out of range")
        }
    }
    mutating func readUInt8() -> UInt8 { take(1, "u8").first! }
    mutating func readInt8() -> Int8 { Int8(bitPattern: readUInt8()) }
    mutating func readUInt16() -> UInt16 {
        var v: UInt16 = 0
        for (i, b) in take(2, "u16").enumerated() { v |= UInt16(b) << (8 * i) }
        return v
    }
    mutating func readInt16() -> Int16 { Int16(bitPattern: readUInt16()) }
    mutating func readUInt32() -> UInt32 {
        var v: UInt32 = 0
        for (i, b) in take(4, "u32").enumerated() { v |= UInt32(b) << (8 * i) }
        return v
    }
    mutating func readInt32() -> Int32 { Int32(bitPattern: readUInt32()) }
    mutating func readUInt64() -> UInt64 {
        var v: UInt64 = 0
        for (i, b) in take(8, "u64").enumerated() { v |= UInt64(b) << (8 * i) }
        return v
    }
    mutating func readInt64() -> Int64 { Int64(bitPattern: readUInt64()) }
    mutating func readFloat() -> Float { Float(bitPattern: readUInt32()) }
    mutating func readDouble() -> Double { Double(bitPattern: readUInt64()) }
    mutating func readLen() -> Int {
        let n = Int(readUInt32())
        guard n <= remaining else { wvDecodeFailure("length prefix exceeds remaining buffer") }
        return n
    }
    mutating func readString() -> String {
        let n = readLen()
        guard let s = String(bytes: take(n, "string bytes"), encoding: .utf8) else {
            wvDecodeFailure("string is not valid UTF-8")
        }
        return s
    }
    mutating func readBytes() -> Data {
        let n = readLen()
        return Data(take(n, "byte buffer"))
    }
    mutating func readOptionFlag() -> Bool {
        switch take(1, "option flag").first! {
        case 0: return false
        case 1: return true
        default: wvDecodeFailure("option flag byte out of range")
        }
    }
    func finish() {
        if remaining != 0 { wvDecodeFailure("trailing bytes after value") }
    }
}

"#;

/// A fresh generated-variable name (`v0`, `n1`, ...) unique within one
/// rendering scope.
fn fresh(counter: &mut usize, prefix: &str) -> String {
    let id = *counter;
    *counter += 1;
    format!("{prefix}{id}")
}

/// Emit statements serializing `expr` (of IR type `ty`) into the `WvWriter`
/// variable named `writer`, recursing through optionals, lists, and maps and
/// delegating records and rich enums to their generated `wvWrite*` codecs.
fn write_value_stmts(
    w: &mut CodeWriter,
    ty: &TypeRef,
    expr: &str,
    writer: &str,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match ty {
        TypeRef::Bool => {
            w.line(format!("{writer}.writeBool({expr})"));
        }
        TypeRef::I8 => {
            w.line(format!("{writer}.writeInt8({expr})"));
        }
        TypeRef::U8 => {
            w.line(format!("{writer}.writeUInt8({expr})"));
        }
        TypeRef::I16 => {
            w.line(format!("{writer}.writeInt16({expr})"));
        }
        TypeRef::U16 => {
            w.line(format!("{writer}.writeUInt16({expr})"));
        }
        TypeRef::I32 => {
            w.line(format!("{writer}.writeInt32({expr})"));
        }
        TypeRef::U32 => {
            w.line(format!("{writer}.writeUInt32({expr})"));
        }
        TypeRef::I64 => {
            w.line(format!("{writer}.writeInt64({expr})"));
        }
        TypeRef::U64 => {
            w.line(format!("{writer}.writeUInt64({expr})"));
        }
        TypeRef::F32 => {
            w.line(format!("{writer}.writeFloat({expr})"));
        }
        TypeRef::F64 => {
            w.line(format!("{writer}.writeDouble({expr})"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{writer}.writeString({expr})"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("{writer}.writeBytes({expr})"));
        }
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            w.line(format!("{writer}.writeUInt64({expr})"));
        }
        TypeRef::Enum(name) => {
            // C-style enums cross as `i32` on the wire; a `UInt32`-raw Swift
            // enum reinterprets its bits.
            if ctx.enum_raw(local_type_name(name)) == "Int32" {
                w.line(format!("{writer}.writeInt32({expr}.rawValue)"));
            } else {
                w.line(format!(
                    "{writer}.writeInt32(Int32(bitPattern: {expr}.rawValue))"
                ));
            }
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            w.line(format!(
                "wvWrite{}({expr}, into: &{writer})",
                local_type_name(name)
            ));
        }
        TypeRef::Optional(inner) => {
            let v = fresh(counter, "v");
            w.line(format!("if let {v} = {expr} {{"));
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(true)"));
            write_value_stmts(w, inner, &v, writer, ctx, counter);
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(false)"));
            w.dedent();
            w.line("}");
        }
        TypeRef::List(inner) => {
            let v = fresh(counter, "v");
            w.line(format!("{writer}.writeLen({expr}.count)"));
            w.line(format!("for {v} in {expr} {{"));
            w.indent();
            write_value_stmts(w, inner, &v, writer, ctx, counter);
            w.dedent();
            w.line("}");
        }
        TypeRef::Map(k, val) => {
            let kv = fresh(counter, "v");
            let vv = fresh(counter, "v");
            w.line(format!("{writer}.writeLen({expr}.count)"));
            w.line(format!("for ({kv}, {vv}) in {expr} {{"));
            w.indent();
            write_value_stmts(w, k, &kv, writer, ctx, counter);
            write_value_stmts(w, val, &vv, writer, ctx, counter);
            w.dedent();
            w.line("}");
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) | TypeRef::Named(_) => {
            unreachable!("type cannot appear inside a value buffer")
        }
    }
}

/// Emit statements deserializing one value of IR type `ty` from the
/// `WvReader` variable named `reader`, binding the result to `out`.
fn read_value_stmts(
    w: &mut CodeWriter,
    ty: &TypeRef,
    out: &str,
    reader: &str,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match ty {
        TypeRef::Bool => {
            w.line(format!("let {out} = {reader}.readBool()"));
        }
        TypeRef::I8 => {
            w.line(format!("let {out} = {reader}.readInt8()"));
        }
        TypeRef::U8 => {
            w.line(format!("let {out} = {reader}.readUInt8()"));
        }
        TypeRef::I16 => {
            w.line(format!("let {out} = {reader}.readInt16()"));
        }
        TypeRef::U16 => {
            w.line(format!("let {out} = {reader}.readUInt16()"));
        }
        TypeRef::I32 => {
            w.line(format!("let {out} = {reader}.readInt32()"));
        }
        TypeRef::U32 => {
            w.line(format!("let {out} = {reader}.readUInt32()"));
        }
        TypeRef::I64 => {
            w.line(format!("let {out} = {reader}.readInt64()"));
        }
        TypeRef::U64 => {
            w.line(format!("let {out} = {reader}.readUInt64()"));
        }
        TypeRef::F32 => {
            w.line(format!("let {out} = {reader}.readFloat()"));
        }
        TypeRef::F64 => {
            w.line(format!("let {out} = {reader}.readDouble()"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("let {out} = {reader}.readString()"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("let {out} = {reader}.readBytes()"));
        }
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            w.line(format!("let {out} = {reader}.readUInt64()"));
        }
        TypeRef::Enum(name) => {
            let local = local_type_name(name);
            let ty_name = ctx.ty_name(local);
            // An unknown discriminant traps, matching the decode-failure
            // channel.
            if ctx.enum_raw(local) == "Int32" {
                w.line(format!(
                    "let {out} = {ty_name}(rawValue: {reader}.readInt32())!"
                ));
            } else {
                w.line(format!(
                    "let {out} = {ty_name}(rawValue: UInt32(bitPattern: {reader}.readInt32()))!"
                ));
            }
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            w.line(format!(
                "let {out} = wvRead{}(&{reader})",
                local_type_name(name)
            ));
        }
        TypeRef::Optional(inner) => {
            let t = swift_type_ctx(inner, ctx);
            w.line(format!("var {out}: {t}? = nil"));
            w.line(format!("if {reader}.readOptionFlag() {{"));
            w.indent();
            let v = fresh(counter, "v");
            read_value_stmts(w, inner, &v, reader, ctx, counter);
            w.line(format!("{out} = {v}"));
            w.dedent();
            w.line("}");
        }
        TypeRef::List(inner) => {
            let t = swift_type_ctx(inner, ctx);
            let cnt = fresh(counter, "n");
            w.line(format!("let {cnt} = {reader}.readLen()"));
            w.line(format!("var {out}: [{t}] = []"));
            w.line(format!("{out}.reserveCapacity({cnt})"));
            w.line(format!("for _ in 0..<{cnt} {{"));
            w.indent();
            let v = fresh(counter, "v");
            read_value_stmts(w, inner, &v, reader, ctx, counter);
            w.line(format!("{out}.append({v})"));
            w.dedent();
            w.line("}");
        }
        TypeRef::Map(k, val) => {
            let kt = swift_type_ctx(k, ctx);
            let vt = swift_type_ctx(val, ctx);
            let cnt = fresh(counter, "n");
            w.line(format!("let {cnt} = {reader}.readLen()"));
            w.line(format!("var {out}: [{kt}: {vt}] = [:]"));
            w.line(format!("{out}.reserveCapacity({cnt})"));
            w.line(format!("for _ in 0..<{cnt} {{"));
            w.indent();
            let kv = fresh(counter, "v");
            let vv = fresh(counter, "v");
            read_value_stmts(w, k, &kv, reader, ctx, counter);
            read_value_stmts(w, val, &vv, reader, ctx, counter);
            w.line(format!("{out}[{kv}] = {vv}"));
            w.dedent();
            w.line("}");
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) | TypeRef::Named(_) => {
            unreachable!("type cannot appear inside a value buffer")
        }
    }
}

/// The raw-value Swift type Swift imports the generated C enum with: a C enum
/// with only non-negative discriminants imports as `UInt32`, otherwise
/// `Int32`. Mirroring the raw type keeps every `.rawValue` round-trip against
/// the C symbols type-correct.
fn enum_raw_type(e: &EnumBinding) -> &'static str {
    if e.variants.iter().any(|v| v.value < 0) {
        "Int32"
    } else {
        "UInt32"
    }
}

fn render_swift_enum(out: &mut String, e: &EnumBinding) {
    let raw = enum_raw_type(e);
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public enum {}: {} {{", e.name, raw));
    w.scope(|w| {
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::TripleSlash);
            w.line(format!(
                "case {} = {}",
                v.name.to_lower_camel_case(),
                v.value
            ));
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as a native Swift enum with associated
/// values: one case per variant, with labeled associated values matching the
/// variant's field names. The value crosses the ABI as a buffer; its codec
/// pair is emitted by [`render_rich_enum_codec`].
fn render_swift_rich_enum(out: &mut String, e: &EnumBinding, ctx: SwiftCtx) {
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public enum {} {{", e.name));
    w.scope(|w| {
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::TripleSlash);
            let case_name = v.name.to_lower_camel_case();
            if v.fields.is_empty() {
                w.line(format!("case {case_name}"));
            } else {
                let assoc = v
                    .fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{}: {}",
                            f.name.to_lower_camel_case(),
                            swift_type_ctx(&f.ty, ctx)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                w.line(format!("case {case_name}({assoc})"));
            }
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the `wvWrite{Name}`/`wvRead{Name}` codec pair for a rich enum: the
/// writer switches on the case, writes the `i32` tag, then the active
/// variant's fields in declaration order; the reader inverts it and traps on
/// an unknown tag.
fn render_rich_enum_codec(out: &mut String, e: &EnumBinding, ctx: SwiftCtx) {
    let local = local_type_name(&e.name);
    let ty_name = ctx.ty_name(local);
    let mut w = CodeWriter::four_space();
    let mut counter = 0usize;

    w.line(format!(
        "/// Serializes a `{local}` into the value-buffer wire format."
    ));
    w.line(format!(
        "func wvWrite{local}(_ value: {ty_name}, into w: inout WvWriter) {{"
    ));
    w.indent();
    w.line("switch value {");
    for v in &e.variants {
        let case_name = v.name.to_lower_camel_case();
        if v.fields.is_empty() {
            w.line(format!("case .{case_name}:"));
            w.indent();
            w.line(format!("w.writeInt32({})", v.value));
            w.dedent();
        } else {
            let binds: Vec<String> = v.fields.iter().map(|_| fresh(&mut counter, "v")).collect();
            w.line(format!("case let .{case_name}({}):", binds.join(", ")));
            w.indent();
            w.line(format!("w.writeInt32({})", v.value));
            for (f, bind) in v.fields.iter().zip(&binds) {
                write_value_stmts(&mut w, &f.ty, bind, "w", ctx, &mut counter);
            }
            w.dedent();
        }
    }
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!(
        "/// Deserializes a `{local}` from the value-buffer wire format."
    ));
    w.line(format!(
        "func wvRead{local}(_ r: inout WvReader) -> {ty_name} {{"
    ));
    w.indent();
    w.line("let tag = r.readInt32()");
    w.line("switch tag {");
    for v in &e.variants {
        let case_name = v.name.to_lower_camel_case();
        w.line(format!("case {}:", v.value));
        w.indent();
        if v.fields.is_empty() {
            w.line(format!("return .{case_name}"));
        } else {
            let mut labeled = Vec::new();
            for f in &v.fields {
                let var = fresh(&mut counter, "v");
                read_value_stmts(&mut w, &f.ty, &var, "r", ctx, &mut counter);
                labeled.push(format!("{}: {var}", f.name.to_lower_camel_case()));
            }
            w.line(format!("return .{case_name}({})", labeled.join(", ")));
        }
        w.dedent();
    }
    w.line("default:");
    w.indent();
    w.line(format!("wvDecodeFailure(\"unknown {local} tag \\(tag)\")"));
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a record as a plain Swift struct: one typed `public var` per field
/// and an explicit public memberwise initializer (the compiler-synthesized
/// one is internal). The value crosses the ABI as a buffer; its codec pair is
/// emitted by [`render_struct_codec`].
fn render_swift_struct(out: &mut String, s: &StructBinding, ctx: SwiftCtx) {
    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public struct {} {{", s.name));
    w.indent();
    for f in &s.fields {
        w.doc(&f.doc, DocCommentStyle::TripleSlash);
        w.line(format!(
            "public var {}: {}",
            f.name.to_lower_camel_case(),
            swift_type_ctx(&f.ty, ctx)
        ));
    }
    w.blank();
    let params = s
        .fields
        .iter()
        .map(|f| {
            format!(
                "{}: {}",
                f.name.to_lower_camel_case(),
                swift_type_ctx(&f.ty, ctx)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    w.line(format!("/// Creates a `{}` value.", s.name));
    w.line(format!("public init({params}) {{"));
    w.scope(|w| {
        for f in &s.fields {
            let prop = f.name.to_lower_camel_case();
            w.line(format!("self.{prop} = {prop}"));
        }
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the `wvWrite{Name}`/`wvRead{Name}` codec pair for a record: fields in
/// declaration order, delegating nested buffered types recursively.
fn render_struct_codec(out: &mut String, s: &StructBinding, ctx: SwiftCtx) {
    let local = local_type_name(&s.name);
    let ty_name = ctx.ty_name(local);
    let mut w = CodeWriter::four_space();
    let mut counter = 0usize;

    w.line(format!(
        "/// Serializes a `{local}` into the value-buffer wire format."
    ));
    w.line(format!(
        "func wvWrite{local}(_ value: {ty_name}, into w: inout WvWriter) {{"
    ));
    w.indent();
    for f in &s.fields {
        let expr = format!("value.{}", f.name.to_lower_camel_case());
        write_value_stmts(&mut w, &f.ty, &expr, "w", ctx, &mut counter);
    }
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!(
        "/// Deserializes a `{local}` from the value-buffer wire format."
    ));
    w.line(format!(
        "func wvRead{local}(_ r: inout WvReader) -> {ty_name} {{"
    ));
    w.indent();
    let mut labeled = Vec::new();
    for f in &s.fields {
        let var = fresh(&mut counter, "v");
        read_value_stmts(&mut w, &f.ty, &var, "r", ctx, &mut counter);
        labeled.push(format!("{}: {var}", f.name.to_lower_camel_case()));
    }
    w.line(format!("return {ty_name}({})", labeled.join(", ")));
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

fn render_swift_wrapper(
    api: &Api,
    model: &BindingModel,
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

    // Index the flat, pre-order model by its underscore-joined symbol path so
    // the recursive IR walk below can pull each module's precomputed C symbols
    // while still emitting the nested Swift `enum` structure the IR tree drives.
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    let all_mods = walk_modules(&api.modules).collect::<Vec<_>>();

    // Every module becomes a namespace `enum`; a wrapper type whose name
    // matches one of these is shadowed inside that namespace and must be
    // module-qualified at its use sites.
    let module_names: HashSet<String> = all_mods
        .iter()
        .map(|m| m.name.to_upper_camel_case())
        .collect();
    // Raw-value types of every C-style enum, so buffer codecs can emit the
    // right `i32` bit conversion for a referenced enum without re-resolving it.
    let enum_raws: HashMap<String, &'static str> = model
        .modules
        .iter()
        .flat_map(|m| m.enums.iter())
        .filter(|e| !e.is_rich())
        .map(|e| (e.name.clone(), enum_raw_type(e)))
        .collect();
    let ctx = SwiftCtx {
        c_prefix,
        swift_module: module_name,
        module_names: &module_names,
        enum_raws: &enum_raws,
    };

    // The generic brand error: unknown codes, marshalling failures, and
    // panics. Typed domain errors get one enum per declaring module, emitted
    // alongside that module's types.
    out.push_str(&format!(
        "public enum {ERROR_BRAND}: Error, LocalizedError {{\n"
    ));
    out.push_str("    case error(code: Int32, message: String)\n");
    out.push_str("    public var errorDescription: String? {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(_, message): return message\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    public var errorCode: Int32 {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(code, _): return code\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc check(_ err: inout weaveffi_error) throws {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    out.push_str(&format!(
        "        throw {ERROR_BRAND}.error(code: code, message: message)\n"
    ));
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // The trapping flavor for non-throwing wrappers: a non-zero code here can
    // only be a producer panic or an argument-marshalling failure.
    out.push_str("@inline(__always)\nfunc trap(_ err: inout weaveffi_error) {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    out.push_str("        fatalError(\"\\(code): \\(message)\")\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // The private buffer runtime implementing the value-buffer wire format.
    out.push_str(BUFFER_RUNTIME);

    // Interface members can be async too, so consult every callable.
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    if has_async {
        // `E` is `Error` for a throwing async wrapper and `Never` for a plain
        // one, mirroring the checked-continuation flavor each uses.
        out.push_str("private final class ContinuationRef<T, E: Error> {\n");
        out.push_str("    let value: CheckedContinuation<T, E>\n");
        out.push_str("    init(_ value: CheckedContinuation<T, E>) { self.value = value }\n");
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
    if let Some(eb) = mb.error.as_ref().filter(|e| e.declared_here) {
        render_swift_error(out, mb, eb, ctx);
    }
    for e in &mb.enums {
        if e.is_rich() {
            // A rich (algebraic) enum is a value type: a native Swift enum
            // with associated values plus its buffer codec pair.
            render_swift_rich_enum(out, e, ctx);
            render_rich_enum_codec(out, e, ctx);
        } else {
            render_swift_enum(out, e);
        }
    }
    for s in &mb.structs {
        // A record is a value type: a plain Swift struct plus its buffer
        // codec pair. Records have no C symbols at all.
        render_swift_struct(out, s, ctx);
        render_struct_codec(out, s, ctx);
    }
    for i in &mb.interfaces {
        render_swift_interface(out, c_prefix, mb, i, ctx);
    }
    // One lazy sequence class per `iter<T>` callable (free functions and
    // interface members alike), emitted at file scope next to the module's
    // other wrapper types.
    for f in mb.callables() {
        if let CallShape::Iterator(it) = &f.shape {
            render_swift_iterator_class(out, mb, f, it, ctx);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_swift_module_types(out, c_prefix, by_path, sub, &sub_path, ctx);
    }
}

/// Escape a string for embedding inside a Swift double-quoted literal.
fn swift_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render one declaring module's typed error surface: a `public enum
/// {TypeName}: Error` whose lowerCamel cases carry the runtime message plus,
/// for codes that declare payload fields, one labeled associated value per
/// field. Also emits the file-scope `map{Stem}` and `check{Stem}` helpers
/// that convert a non-zero `weaveffi_error` slot into it, decoding the
/// payload buffer for codes with fields (unknown codes fall back to the
/// generic [`ERROR_BRAND`]).
fn render_swift_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding, ctx: SwiftCtx) {
    let stem = eb.owner_path.to_upper_camel_case();
    let ty = &eb.type_name;

    let case_decl = |fields: &[FieldBinding]| -> String {
        let mut parts = vec!["message: String".to_string()];
        for f in fields {
            parts.push(format!(
                "{}: {}",
                f.name.to_lower_camel_case(),
                swift_type_ctx(&f.ty, ctx)
            ));
        }
        parts.join(", ")
    };

    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Typed errors reported by the `{}` module.",
            module.segments.join(".")
        )),
        DocCommentStyle::TripleSlash,
    );
    w.line(format!("public enum {ty}: Error, LocalizedError {{"));
    w.indent();
    for c in &eb.codes {
        w.doc(&c.doc, DocCommentStyle::TripleSlash);
        w.line(format!(
            "case {}({})",
            c.name.to_lower_camel_case(),
            case_decl(&c.fields)
        ));
    }
    w.line("public var errorDescription: String? {");
    w.scope(|w| {
        w.line("switch self {");
        for c in &eb.codes {
            // Bind only the message; wildcard the payload fields.
            let mut binds = vec!["message".to_string()];
            binds.extend(c.fields.iter().map(|_| "_".to_string()));
            w.line(format!(
                "case let .{}({}): return message",
                c.name.to_lower_camel_case(),
                binds.join(", ")
            ));
        }
        w.line("}");
    });
    w.line("}");
    w.line("/// The numeric ABI code carried by this error.");
    w.line("public var errorCode: Int32 {");
    w.scope(|w| {
        w.line("switch self {");
        for c in &eb.codes {
            w.line(format!(
                "case .{}: return {}",
                c.name.to_lower_camel_case(),
                c.value
            ));
        }
        w.line("}");
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    // `map{Stem}`: code -> typed case (default message when the slot carried
    // none), decoding the payload buffer for codes that declare fields.
    // Unknown code -> generic brand error.
    w.line("@inline(__always)");
    w.line(format!(
        "func map{stem}(code: Int32, message: String, payload: [UInt8]?) -> Error {{"
    ));
    w.indent();
    w.line("switch code {");
    for c in &eb.codes {
        let case_name = c.name.to_lower_camel_case();
        let message_arg = format!(
            "message: message.isEmpty ? \"{}\" : message",
            swift_str(&c.message)
        );
        if c.fields.is_empty() {
            w.line(format!(
                "case {}: return {ty}.{case_name}({message_arg})",
                c.value
            ));
        } else {
            w.line(format!("case {}:", c.value));
            w.indent();
            w.line("var payloadReader = WvReader(bytes: payload ?? [])");
            let mut counter = 0usize;
            let mut args = vec![message_arg];
            for f in &c.fields {
                let var = fresh(&mut counter, "v");
                read_value_stmts(&mut w, &f.ty, &var, "payloadReader", ctx, &mut counter);
                args.push(format!("{}: {var}", f.name.to_lower_camel_case()));
            }
            w.line("payloadReader.finish()");
            w.line(format!("return {ty}.{case_name}({})", args.join(", ")));
            w.dedent();
        }
    }
    w.line(format!(
        "default: return {ERROR_BRAND}.error(code: code, message: message)"
    ));
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    w.line("@inline(__always)");
    w.line(format!(
        "func check{stem}(_ err: inout weaveffi_error) throws {{"
    ));
    w.indent();
    w.line("if err.code != 0 {");
    w.scope(|w| {
        w.line("let code = err.code");
        w.line("let message = err.message.flatMap { String(cString: $0) } ?? \"\"");
        w.line("let payload: [UInt8]? = err.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.payload_len)) }");
        w.line("weaveffi_error_clear(&err)");
        w.line(format!(
            "throw map{stem}(code: code, message: message, payload: payload)"
        ));
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Clone a callable with its parameter names camel-cased, so the Swift
/// argument labels, bound locals, and every staged `_ptr`/`_len` variable
/// derived from them agree.
fn camel_params(f: &FnBinding) -> FnBinding {
    let mut f = f.clone();
    for p in &mut f.params {
        p.name = p.name.to_lower_camel_case();
    }
    f
}

/// Render one interface as a `public final class` owning its C handle: a
/// stored `ptr`, an internal ownership-adopting `init(ptr:)`, and a `deinit`
/// that calls the destroy symbol.
///
/// The constructor named `new` surfaces as `public init` (throwing when the
/// IDL marks it `throws`); every other constructor becomes a `public static
/// func` factory. Methods are instance funcs that pass `ptr` as the leading C
/// argument; statics are plain `public static func`s. Member bodies reuse the
/// free-function marshalling paths.
fn render_swift_interface(
    out: &mut String,
    c_prefix: &str,
    module: &ModuleBinding,
    iface: &InterfaceBinding,
    ctx: SwiftCtx,
) {
    let stem = domain_stem(module);
    let class_name = local_type_name(&iface.name);

    let mut w = CodeWriter::four_space();
    w.doc(&iface.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public final class {class_name} {{"));
    w.indent();
    w.line("let ptr: OpaquePointer");
    w.blank();
    w.line("init(ptr: OpaquePointer) {");
    w.scope(|w| {
        w.line("self.ptr = ptr");
    });
    w.line("}");
    w.blank();
    w.line("deinit {");
    w.scope(|w| {
        w.line(format!("{}(ptr)", iface.destroy_symbol));
    });
    w.line("}");
    w.dedent();

    let mut members = String::new();
    for c in &iface.constructors {
        let f = camel_params(c);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        if f.name == "new" {
            render_swift_ctor_init(&mut members, c_prefix, &module.path, &f, err, ctx);
        } else {
            let swift_name = f.name.to_lower_camel_case();
            render_swift_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        }
    }
    for m in &iface.methods {
        let f = camel_params(m);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        let swift_name = f.name.to_lower_camel_case();
        if matches!(f.shape, CallShape::Async(_)) {
            render_swift_async_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                Some("ptr"),
                err,
                ctx,
            );
        } else {
            render_swift_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                Some("ptr"),
                err,
                ctx,
            );
        }
    }
    for s in &iface.statics {
        let f = camel_params(s);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        let swift_name = f.name.to_lower_camel_case();
        if matches!(f.shape, CallShape::Async(_)) {
            render_swift_async_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        } else {
            render_swift_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        }
    }
    w.raw(members);

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render the constructor named `new` as `public init`: the body is the
/// shared call body with an assign-to-`self.ptr` tail instead of a
/// wrapper-returning one. Throwing before `self.ptr` is assigned is legal in
/// a root-class initializer, so the error paths carry over unchanged.
fn render_swift_ctor_init(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    err: ErrCtx,
    ctx: SwiftCtx,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, "    ");
        w.raw(tmp);
    }
    let mut sig = String::new();
    write_swift_params_sig(&mut sig, &f.params, ctx);
    let throws_kw = if err.throws { " throws" } else { "" };
    w.line(format!("public init({sig}){throws_kw} {{"));
    w.indent();
    render_call_body(&mut w, f, c_prefix, module_name, None, err, ctx, true);
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
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
    let stem = domain_stem(mb);
    let mut bodies: Vec<String> = Vec::new();
    for l in &mb.listeners {
        let mut buf = String::new();
        render_swift_listener(&mut buf, module_path, mb, l, strip_module_prefix, ctx);
        bodies.push(buf);
    }
    for f in &mb.functions {
        let mut buf = String::new();
        let f = camel_params(f);
        let swift_name =
            wrapper_name(module_path, &f.name, strip_module_prefix).to_lower_camel_case();
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        if matches!(f.shape, CallShape::Async(_)) {
            render_swift_async_function(
                &mut buf,
                c_prefix,
                module_path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        } else {
            render_swift_function(
                &mut buf,
                c_prefix,
                module_path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
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

/// Prepend an instance receiver's pointer to a rendered C argument list.
fn with_self_arg(call_args: String, self_arg: Option<&str>) -> String {
    match self_arg {
        Some(recv) if call_args.is_empty() => recv.to_string(),
        Some(recv) => format!("{recv}, {call_args}"),
        None => call_args,
    }
}

/// Render one synchronous (or iterator-returning) callable. `swift_name` is
/// the already-cased wrapper name; `self_arg` is `Some("ptr")` for an
/// instance method, making the wrapper a member `func` that passes its own
/// handle as the leading C argument.
#[allow(clippy::too_many_arguments)]
fn render_swift_function(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    swift_name: &str,
    self_arg: Option<&str>,
    err: ErrCtx,
    ctx: SwiftCtx,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, "    ");
        w.raw(tmp);
    }
    if let CallShape::Iterator(_) = &f.shape {
        w.line("/// - Returns: A lazy sequence that pulls one element per step from the");
        w.line("///   producer; the underlying iterator is destroyed when the sequence is");
        w.line("///   exhausted or deinitialized.");
        if err.throws {
            w.line("/// - Throws: The module's typed error if the launch fails. Mid-stream");
            w.line("///   errors end iteration and are stored in the sequence's `error`");
            w.line("///   property instead of being thrown.");
        }
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "@available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        ));
    }
    let ret_swift = match &f.shape {
        CallShape::Iterator(it) => ctx.ty_name(&iterator_class_name(it, c_prefix)),
        _ => f
            .ret
            .as_ref()
            .map(|t| swift_type_ctx(t, ctx))
            .unwrap_or_else(|| "Void".to_string()),
    };
    let mut sig = String::new();
    write_swift_params_sig(&mut sig, &f.params, ctx);
    let static_kw = if self_arg.is_some() { "" } else { "static " };
    let throws_kw = if err.throws { " throws" } else { "" };
    w.line(format!(
        "public {static_kw}func {swift_name}({sig}){throws_kw} -> {ret_swift} {{"
    ));
    w.indent();
    render_call_body(&mut w, f, c_prefix, module_name, self_arg, err, ctx, false);
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// Write `name: SwiftType, name: SwiftType, ...` directly into `out`,
/// avoiding the per-call `format!` and intermediate `Vec<String>` allocations
/// that `params.iter().map(format!).collect::<Vec<_>>().join(", ")` would
/// require. Parameters carry real argument labels (their camel-cased names).
fn write_swift_params_sig(out: &mut String, params: &[ParamBinding], ctx: SwiftCtx) {
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}: {}", p.name, swift_type_ctx(&p.ty, ctx));
    }
}

/// The fully-prefixed C type name of a C-style enum referenced (possibly
/// cross-module) from `module_name`.
fn c_enum_type(name: &str, c_prefix: &str, module_name: &str) -> String {
    let (module, local) = split_qualified(name, module_name);
    format!("{c_prefix}_{module}_{local}")
}

/// Render the C argument list for `params`: staged params contribute their
/// `_ptr`/`_len` bindings, direct params their converted expressions.
fn build_c_call_args(params: &[ParamBinding], c_prefix: &str, module_name: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in params {
        match staging_for(&p.ty) {
            // Strings are a single NUL-terminated `const char*`.
            Staging::CString => args.push(format!("{}_ptr", p.name)),
            // Bytes and buffered values pass an explicit (ptr, len) pair.
            Staging::Bytes | Staging::Buffered => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            Staging::Direct => match &p.ty {
                // An interface param borrows the wrapper's handle for the
                // call; the receiver stays alive for the call frame.
                TypeRef::Interface(_) => args.push(format!("{}.ptr", p.name)),
                TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)) => {
                    args.push(format!("{}?.ptr", p.name));
                }
                TypeRef::Enum(enum_name) => args.push(format!(
                    "{}({}.rawValue)",
                    c_enum_type(enum_name, c_prefix, module_name),
                    p.name
                )),
                // A typed handle is a `UInt64` token in Swift; the C slot is
                // an opaque typed pointer, so reinterpret the bits.
                TypeRef::TypedHandle(_) => {
                    args.push(format!("OpaquePointer(bitPattern: UInt({}))", p.name));
                }
                _ => args.push(p.name.clone()),
            },
        }
    }
    args.join(", ")
}

/// The Swift spelling of the raw C return value of `f`, used to annotate the
/// binding when the call sits inside multi-statement staging closures (whose
/// return type Swift cannot infer).
fn raw_return_swift(f: &FnBinding, c_prefix: &str, module_name: &str) -> String {
    match f.ret.as_ref() {
        None => "Void".to_string(),
        Some(ty) if abi::is_buffered(ty) => "UnsafePointer<UInt8>?".to_string(),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => "UnsafePointer<CChar>?".to_string(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => "UnsafePointer<UInt8>?".to_string(),
        Some(
            TypeRef::Iterator(_)
            | TypeRef::Interface(_)
            | TypeRef::TypedHandle(_)
            // Only `Interface?` reaches here (every other optional is
            // buffered): a nullable owned object pointer.
            | TypeRef::Optional(_),
        ) => "OpaquePointer?".to_string(),
        Some(TypeRef::Enum(name)) => c_enum_type(name, c_prefix, module_name),
        Some(other) => swift_type_for(other),
    }
}

/// Render the shared body of a synchronous callable: the error slot, input
/// staging (byte copies and buffer packing), the C call wrapped in whatever
/// pointer-staging closures the inputs need, the error check, and the return
/// conversion. With `ctor` set, an interface-returning tail assigns
/// `self.ptr` instead of wrapping the pointer.
#[allow(clippy::too_many_arguments)]
fn render_call_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    c_prefix: &str,
    module_name: &str,
    self_arg: Option<&str>,
    err: ErrCtx,
    ctx: SwiftCtx,
    ctor: bool,
) {
    let mut counter = 0usize;
    w.line("var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)");

    // Staging: byte params are copied into `[UInt8]`, buffered params are
    // packed into a writer; both are handed to the C call via
    // `withUnsafeBufferPointer` below.
    for p in &f.params {
        match staging_for(&p.ty) {
            Staging::Bytes => {
                w.line(format!("let {n}Bytes = Array({n})", n = p.name));
            }
            Staging::Buffered => {
                w.line(format!("var {n}Writer = WvWriter()", n = p.name));
                let writer = format!("{}Writer", p.name);
                write_value_stmts(w, &p.ty, &p.name, &writer, ctx, &mut counter);
            }
            _ => {}
        }
    }

    let needs_out_len = matches!(
        f.ret.as_ref(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes)
    ) || f.ret.as_ref().is_some_and(abi::is_buffered);
    if needs_out_len {
        w.line("var outLen: Int = 0");
    }

    let c_sym = &f.c_base;
    let mut all_args = with_self_arg(
        build_c_call_args(&f.params, c_prefix, module_name),
        self_arg,
    );
    if needs_out_len {
        if all_args.is_empty() {
            all_args.push_str("&outLen");
        } else {
            all_args.push_str(", &outLen");
        }
    }
    let call = if all_args.is_empty() {
        format!("{c_sym}(&err)")
    } else {
        format!("{c_sym}({all_args}, &err)")
    };

    let closure_params: Vec<&ParamBinding> = f
        .params
        .iter()
        .filter(|p| staging_for(&p.ty) != Staging::Direct)
        .collect();
    let has_ret = f.ret.is_some();

    if closure_params.is_empty() {
        if has_ret {
            w.line(format!("let rv = {call}"));
        } else {
            w.line(call);
        }
    } else {
        let raw_ty = raw_return_swift(f, c_prefix, module_name);
        for (i, p) in closure_params.iter().enumerate() {
            let bind = if !has_ret {
                String::new()
            } else if i == 0 {
                format!("let rv: {raw_ty} = ")
            } else {
                "return ".to_string()
            };
            let n = &p.name;
            match staging_for(&p.ty) {
                Staging::CString => {
                    w.line(format!("{bind}{n}.withCString {{ {n}_ptr in"));
                    w.indent();
                }
                Staging::Bytes => {
                    w.line(format!(
                        "{bind}{n}Bytes.withUnsafeBufferPointer {{ {n}_buf in"
                    ));
                    w.indent();
                    w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                    w.line(format!("let {n}_len = {n}_buf.count"));
                }
                Staging::Buffered => {
                    w.line(format!(
                        "{bind}{n}Writer.bytes.withUnsafeBufferPointer {{ {n}_buf in"
                    ));
                    w.indent();
                    w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                    w.line(format!("let {n}_len = {n}_buf.count"));
                }
                Staging::Direct => unreachable!(),
            }
        }
        if has_ret {
            w.line(format!("return {call}"));
        } else {
            w.line(call);
        }
        for _ in 0..closure_params.len() {
            w.dedent();
            w.line("}");
        }
    }

    w.line(err.check_stmt("err"));
    render_return_tail(w, f, err, ctx, ctor, &mut counter);
}

/// Render the post-check return conversion of a callable body, consuming the
/// raw call result bound as `rv`.
fn render_return_tail(
    w: &mut CodeWriter,
    f: &FnBinding,
    err: ErrCtx,
    ctx: SwiftCtx,
    ctor: bool,
    counter: &mut usize,
) {
    match f.ret.as_ref() {
        None => {}
        Some(ty) if abi::is_buffered(ty) => {
            // Copy the encoding, release the producer buffer, then decode.
            w.line(err.guard_stmt("rv", -1, "null buffer"));
            w.line("let rvBytes = [UInt8](UnsafeBufferPointer(start: rv, count: outLen))");
            w.line("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)");
            w.line("var rvReader = WvReader(bytes: rvBytes)");
            let v = fresh(counter, "v");
            read_value_stmts(w, ty, &v, "rvReader", ctx, counter);
            w.line("rvReader.finish()");
            w.line(format!("return {v}"));
        }
        Some(TypeRef::Iterator(_)) => {
            let CallShape::Iterator(it) = &f.shape else {
                unreachable!("iterator return implies iterator shape")
            };
            let class_name = ctx.ty_name(&iterator_class_name(it, ctx.c_prefix));
            w.line(err.guard_stmt("rv", -1, "null iterator"));
            w.line(format!("return {class_name}(handle: rv)"));
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            w.line(err.guard_stmt("rv", -1, "null string"));
            w.line("defer { weaveffi_free_string(rv) }");
            w.line("return String(cString: rv)");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            w.line("guard let rv = rv else { return Data() }");
            w.line("defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen) }");
            w.line("return Data(bytes: rv, count: outLen)");
        }
        Some(TypeRef::Enum(name)) => {
            let ty_name = ctx.ty_name(local_type_name(name));
            w.line(format!("return {ty_name}(rawValue: rv.rawValue)!"));
        }
        Some(TypeRef::Interface(name)) => {
            w.line(err.guard_stmt("rv", -1, "null pointer"));
            if ctor {
                w.line("self.ptr = rv");
            } else {
                w.line(format!(
                    "return {}(ptr: rv)",
                    ctx.ty_name(local_type_name(name))
                ));
            }
        }
        // Only `Interface?` reaches here (every other optional is buffered):
        // null means none.
        Some(TypeRef::Optional(inner)) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("non-interface optional is buffered")
            };
            w.line(format!(
                "return rv.map {{ {}(ptr: $0) }}",
                ctx.ty_name(local_type_name(name))
            ));
        }
        Some(TypeRef::TypedHandle(_)) => {
            w.line("return UInt64(UInt(bitPattern: rv))");
        }
        Some(_) => {
            w.line("return rv");
        }
    }
}

/// The Swift type one callback parameter surfaces as in the user closure.
/// Interface parameters stay raw (`OpaquePointer?`): wrapping them in the
/// owning Swift class would `*_destroy` a borrowed handle on ARC release.
/// Buffered parameters are decoded to their idiomatic value types before the
/// closure is invoked.
fn swift_cb_param_type(ty: &TypeRef, ctx: SwiftCtx) -> String {
    match ty {
        TypeRef::Interface(_) => "OpaquePointer?".into(),
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)) => {
            "OpaquePointer?".into()
        }
        other => swift_type_ctx(other, ctx),
    }
}

/// The expression converting one *direct* callback parameter's C slots into
/// the value handed to the user closure. Slot names follow
/// [`abi::lower_param`]. Buffered parameters are decoded via statements
/// instead (see [`render_swift_listener`]).
fn swift_cb_direct_arg(p: &ParamBinding, ctx: SwiftCtx) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = slots[0].name.clone();
    match &p.ty {
        TypeRef::Enum(_) => {
            let local = swift_type_ctx(&p.ty, ctx);
            format!("{local}(rawValue: {n0}.rawValue)!")
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("String(cString: {n0}!)"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = &slots[1].name;
            format!("{n0}.map {{ Data(bytes: $0, count: {n1}) }} ?? Data()")
        }
        TypeRef::TypedHandle(_) => format!("UInt64(UInt(bitPattern: {n0}))"),
        // Interfaces (and nullable interfaces) stay raw borrowed pointers.
        _ => n0,
    }
}

/// The register/unregister pair for one listener. The user closure is boxed
/// (`WvCallbackBox`) and retained through the C `context` pointer; the
/// capture-free trampoline closure decodes any buffered arguments, unboxes
/// the user closure, and invokes it.
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
    )
    .to_lower_camel_case();
    let unregister_fn = wrapper_name(
        module_path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_lower_camel_case();

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

    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &l.doc, &[], "    ");
        w.raw(tmp);
    }
    w.line(format!(
        "/// - Returns: A subscription id for ``{unregister_fn}(_:)``."
    ));
    w.line(format!(
        "public static func {register_fn}(_ callback: @escaping {closure_type}) -> UInt64 {{"
    ));
    w.indent();
    w.line("let box = WvCallbackBox(callback)");
    w.line("let ctx = Unmanaged.passRetained(box).toOpaque()");
    w.line(format!(
        "let id = {}({{ {} in",
        l.register_symbol,
        slot_names.join(", ")
    ));
    w.indent();
    w.line(format!(
        "let cb = Unmanaged<WvCallbackBox<{closure_type}>>.fromOpaque(context!).takeUnretainedValue().value"
    ));
    // Buffered arguments are borrowed (ptr, len) pairs, valid only for the
    // dispatch: decode them before invoking the user closure.
    let mut counter = 0usize;
    let mut args: Vec<String> = Vec::new();
    for p in &cb.params {
        if abi::is_buffered(&p.ty) {
            let slots = abi::lower_param(&p.name, &p.ty, "", false);
            let base = p.name.to_lower_camel_case();
            w.line(format!(
                "let {base}Buf = [UInt8](UnsafeBufferPointer(start: {}, count: {}))",
                slots[0].name, slots[1].name
            ));
            w.line(format!("var {base}Reader = WvReader(bytes: {base}Buf)"));
            let v = fresh(&mut counter, "v");
            let reader = format!("{base}Reader");
            read_value_stmts(&mut w, &p.ty, &v, &reader, ctx, &mut counter);
            w.line(format!("{base}Reader.finish()"));
            args.push(v);
        } else {
            args.push(swift_cb_direct_arg(p, ctx));
        }
    }
    w.line(format!("cb({})", args.join(", ")));
    w.dedent();
    w.line("}, ctx)");
    w.line("wvListenerLock.lock()");
    w.line("wvListenerContexts[id] = ctx");
    w.line("wvListenerLock.unlock()");
    w.line("return id");
    w.dedent();
    w.line("}");

    w.line(format!(
        "/// Unregisters a listener previously registered with ``{register_fn}(_:)``."
    ));
    w.line(format!(
        "public static func {unregister_fn}(_ id: UInt64) {{"
    ));
    w.indent();
    w.line(format!("{}(id)", l.unregister_symbol));
    w.line("wvListenerLock.lock()");
    w.line("let ctx = wvListenerContexts.removeValue(forKey: id)");
    w.line("wvListenerLock.unlock()");
    w.line("if let ctx = ctx {");
    w.scope(|w| {
        w.line(format!(
            "Unmanaged<WvCallbackBox<{closure_type}>>.fromOpaque(ctx).release()"
        ));
    });
    w.line("}");
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// Render one async callable as a continuation-backed `async` wrapper. A
/// throwing callable is `async throws` over a throwing continuation resuming
/// the module's typed error; a plain one is `async` over a never-throwing
/// continuation that traps on the (panic-only) error path. `self_arg` works
/// as in [`render_swift_function`].
#[allow(clippy::too_many_arguments)]
fn render_swift_async_function(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    swift_name: &str,
    self_arg: Option<&str>,
    err: ErrCtx,
    ctx: SwiftCtx,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, "    ");
        w.raw(tmp);
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "@available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        ));
    }
    let ret_swift = f
        .ret
        .as_ref()
        .map(|t| swift_type_ctx(t, ctx))
        .unwrap_or_else(|| "Void".to_string());
    let err_ty = err.continuation_err_ty();
    let mut sig = String::new();
    write_swift_params_sig(&mut sig, &f.params, ctx);
    let static_kw = if self_arg.is_some() { "" } else { "static " };
    if err.throws {
        w.line(format!(
            "public {static_kw}func {swift_name}({sig}) async throws -> {ret_swift} {{"
        ));
        w.indent();
        w.line(format!(
            "try await withCheckedThrowingContinuation {{ (continuation: CheckedContinuation<{ret_swift}, Error>) in"
        ));
    } else {
        w.line(format!(
            "public {static_kw}func {swift_name}({sig}) async -> {ret_swift} {{"
        ));
        w.indent();
        w.line(format!(
            "await withCheckedContinuation {{ (continuation: CheckedContinuation<{ret_swift}, Never>) in"
        ));
    }
    w.indent();
    w.line("let ctx = Unmanaged.passRetained(ContinuationRef(continuation)).toOpaque()");

    // Staging: identical to the sync path. The producer copies every input
    // synchronously during the launch, so pointer validity for the launch
    // call's duration is sufficient.
    let mut counter = 0usize;
    for p in &f.params {
        match staging_for(&p.ty) {
            Staging::Bytes => {
                w.line(format!("let {n}Bytes = Array({n})", n = p.name));
            }
            Staging::Buffered => {
                w.line(format!("var {n}Writer = WvWriter()", n = p.name));
                let writer = format!("{}Writer", p.name);
                write_value_stmts(&mut w, &p.ty, &p.name, &writer, ctx, &mut counter);
            }
            _ => {}
        }
    }

    // The launch returns void, so the staging closures carry no binding.
    let closure_params: Vec<&ParamBinding> = f
        .params
        .iter()
        .filter(|p| staging_for(&p.ty) != Staging::Direct)
        .collect();
    for p in &closure_params {
        let n = &p.name;
        match staging_for(&p.ty) {
            Staging::CString => {
                w.line(format!("{n}.withCString {{ {n}_ptr in"));
                w.indent();
            }
            Staging::Bytes => {
                w.line(format!("{n}Bytes.withUnsafeBufferPointer {{ {n}_buf in"));
                w.indent();
                w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                w.line(format!("let {n}_len = {n}_buf.count"));
            }
            Staging::Buffered => {
                w.line(format!(
                    "{n}Writer.bytes.withUnsafeBufferPointer {{ {n}_buf in"
                ));
                w.indent();
                w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                w.line(format!("let {n}_len = {n}_buf.count"));
            }
            Staging::Direct => unreachable!(),
        }
    }

    let c_sym = format!("{}_async", f.c_base);
    let call_args = with_self_arg(
        build_c_call_args(&f.params, c_prefix, module_name),
        self_arg,
    );
    let cb_param_names = async_callback_param_names(&f.ret);

    let mut launch_prefix = String::new();
    if !call_args.is_empty() {
        launch_prefix.push_str(&call_args);
        launch_prefix.push_str(", ");
    }
    if f.cancellable {
        launch_prefix.push_str("nil, ");
    }
    w.line(format!("{c_sym}({launch_prefix}{{ {cb_param_names} in"));
    w.indent();
    w.line(format!(
        "let contRef = Unmanaged<ContinuationRef<{ret_swift}, {err_ty}>>.fromOpaque(context!).takeRetainedValue()"
    ));
    w.line("if let err = err, err.pointee.code != 0 {");
    w.indent();
    w.line("let code = err.pointee.code");
    w.line("let msg = err.pointee.message.flatMap { String(cString: $0) } ?? \"\"");
    for line in err.async_err_lines() {
        w.line(line);
    }
    w.dedent();
    w.line("} else {");
    w.indent();
    render_async_resume_result(&mut w, &f.ret, err, ctx, &mut counter);
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}, ctx)");

    for _ in 0..closure_params.len() {
        w.dedent();
        w.line("}");
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

fn async_callback_param_names(returns: &Option<TypeRef>) -> &'static str {
    match returns {
        None => "context, err",
        Some(ty) if abi::is_buffered(ty) => "context, err, resultPtr, resultLen",
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => "context, err, result, resultLen",
        Some(_) => "context, err, result",
    }
}

/// Render the success branch of an async completion callback: convert the
/// callback's result slots and resume the continuation exactly once.
///
/// Result buffers (strings, bytes, and buffered values) are borrowed for the
/// callback's duration: they're deep-copied or decoded before the callback
/// returns and never freed here. Owned-object results are the exception; the
/// callback receives ownership and the pointer is adopted by its wrapper
/// class.
fn render_async_resume_result(
    w: &mut CodeWriter,
    returns: &Option<TypeRef>,
    err: ErrCtx,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match returns {
        None => {
            w.line("contRef.value.resume(returning: ())");
        }
        Some(ty) if abi::is_buffered(ty) => {
            // Borrowed for the callback's duration: copy the bytes and decode
            // inside the callback; the producer frees its own buffer after.
            w.line("guard let resultPtr = resultPtr else {");
            w.indent();
            w.line(err.async_fail_stmt(-1, "null buffer"));
            if err.throws {
                w.line("return");
            }
            w.dedent();
            w.line("}");
            w.line(
                "let resultBytes = [UInt8](UnsafeBufferPointer(start: resultPtr, count: resultLen))",
            );
            w.line("var resultReader = WvReader(bytes: resultBytes)");
            let v = fresh(counter, "v");
            read_value_stmts(w, ty, &v, "resultReader", ctx, counter);
            w.line("resultReader.finish()");
            w.line(format!("contRef.value.resume(returning: {v})"));
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            w.line("guard let result = result else {");
            w.indent();
            w.line(err.async_fail_stmt(-1, "null string"));
            // `fatalError` already never returns; only the resuming
            // (throwing) flavor needs an explicit exit from the guard.
            if err.throws {
                w.line("return");
            }
            w.dedent();
            w.line("}");
            // The string is borrowed for the callback's duration: copy it,
            // don't free it (the producer releases its own buffer).
            w.line("contRef.value.resume(returning: String(cString: result))");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            w.line("if let result = result {");
            w.scope(|w| {
                w.line("contRef.value.resume(returning: Data(bytes: result, count: resultLen))");
            });
            w.line("} else {");
            w.scope(|w| {
                w.line("contRef.value.resume(returning: Data())");
            });
            w.line("}");
        }
        Some(TypeRef::Enum(name)) => {
            let ty_name = ctx.ty_name(local_type_name(name));
            w.line(format!(
                "contRef.value.resume(returning: {ty_name}(rawValue: result.rawValue)!)"
            ));
        }
        // An owned interface result is adopted: the consumer owns it and the
        // wrapper's deinit calls `_destroy`.
        Some(TypeRef::Interface(name)) => {
            let ty_name = ctx.ty_name(local_type_name(name));
            w.line("guard let result = result else {");
            w.indent();
            w.line(err.async_fail_stmt(-1, "null pointer"));
            if err.throws {
                w.line("return");
            }
            w.dedent();
            w.line("}");
            w.line(format!(
                "contRef.value.resume(returning: {ty_name}(ptr: result))"
            ));
        }
        // Only `Interface?` reaches here: null means none.
        Some(TypeRef::Optional(inner)) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("non-interface optional is buffered")
            };
            let ty_name = ctx.ty_name(local_type_name(name));
            w.line(format!(
                "contRef.value.resume(returning: result.map {{ {ty_name}(ptr: $0) }})"
            ));
        }
        Some(TypeRef::TypedHandle(_)) => {
            w.line("contRef.value.resume(returning: UInt64(UInt(bitPattern: result)))");
        }
        // Validation rejects `async` functions returning `iter<T>` (the
        // callback-completed ABI has no streaming protocol).
        Some(TypeRef::Iterator(_)) => {
            unreachable!("async functions cannot return iterators")
        }
        Some(_) => {
            w.line("contRef.value.resume(returning: result)");
        }
    }
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

/// The Swift name of the lazy sequence class emitted for one `iter<T>`
/// function: the iterator tag minus the C prefix, PascalCased
/// (`weaveffi_kv_ScanIterator` becomes `KvScanIterator`).
fn iterator_class_name(it: &IteratorBinding, c_prefix: &str) -> String {
    it.iter_tag
        .strip_prefix(&format!("{c_prefix}_"))
        .unwrap_or(&it.iter_tag)
        .to_upper_camel_case()
}

/// Emit the lazy sequence class backing one `iter<T>` function.
///
/// The class conforms to `Sequence & IteratorProtocol` and owns the C
/// iterator handle. Each `next()` issues exactly one producer `next` call;
/// the handle is destroyed eagerly on exhaustion (or on a mid-stream error)
/// and again, guarded against double-destroy by the nulled handle, from
/// `deinit` when iteration is abandoned early. Elements are converted and
/// released per the [`weaveffi_core::plan::elem_free`] contract: strings are
/// copied then freed, bytes and buffered elements are copied or decoded then
/// released with `weaveffi_free_bytes`, owned interface pointers are adopted
/// by their wrapper classes, and by-value elements need no release.
///
/// Errors follow the owning function's [`ErrorStrategy`]. `next()` cannot
/// throw under `IteratorProtocol`, so for a throwing function a mid-stream
/// domain error ends iteration and is stored in the sequence's public
/// `error` property for the caller to inspect; for a non-throwing function
/// a reported error can only be a producer bug and traps via `fatalError`.
fn render_swift_iterator_class(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    it: &IteratorBinding,
    ctx: SwiftCtx,
) {
    let protocol = it.protocol(f);
    let class_name = iterator_class_name(it, ctx.c_prefix);
    let next_fn = &it.next.symbol;
    let destroy_fn = &it.destroy_symbol;
    let inner = &it.elem;
    let elem_swift = swift_type_ctx(inner, ctx);
    let stem = domain_stem(mb);
    let throws = protocol.error == ErrorStrategy::Throws;
    let is_buffered_elem = abi::is_buffered(inner);
    let is_bytes_elem = matches!(inner, TypeRef::Bytes | TypeRef::BorrowedBytes);
    let has_len_slot = protocol.elem_free == ElemFree::Bytes;

    // `out_item` is the slot after the iterator handle; render its pointee as
    // the element C type so enum slots get the imported C enum
    // (`{prefix}_{module}_{Name}`).
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

    // The `out_item` slot declaration.
    let (c_var, default): (String, String) = match inner {
        _ if has_len_slot => ("UnsafePointer<UInt8>?".to_string(), "nil".to_string()),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            ("UnsafePointer<CChar>?".to_string(), "nil".to_string())
        }
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) | TypeRef::Optional(_) => {
            ("OpaquePointer?".to_string(), "nil".to_string())
        }
        TypeRef::Enum(_) => (elem_c_type.clone(), format!("{elem_c_type}(0)")),
        _ => (swift_type_for(inner), swift_scalar_default(inner)),
    };

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "/// A lazy sequence over the `{elem_swift}` elements streamed by `{}`.",
        it.launch.symbol
    ));
    w.line("///");
    w.line("/// Each `next()` call pulls exactly one element from the producer. The");
    w.line("/// underlying C iterator is destroyed eagerly on exhaustion and from");
    w.line("/// `deinit` when iteration is abandoned early.");
    if throws {
        w.line("///");
        w.line("/// If the producer reports an error mid-stream, iteration ends and the");
        w.line("/// error is stored in ``error`` for the caller to inspect after the loop.");
    }
    w.line(format!(
        "public final class {class_name}: Sequence, IteratorProtocol {{"
    ));
    w.indent();
    w.line("private var handle: OpaquePointer?");
    if throws {
        w.line("/// The error that ended iteration early, if any.");
        w.line("public private(set) var error: Error?");
    }
    w.blank();
    w.line("init(handle: OpaquePointer) {");
    w.scope(|w| {
        w.line("self.handle = handle");
    });
    w.line("}");
    w.blank();
    w.line("deinit {");
    w.scope(|w| {
        w.line("destroyHandle()");
    });
    w.line("}");
    w.blank();
    w.line("private func destroyHandle() {");
    w.scope(|w| {
        w.line("guard let handle = handle else { return }");
        w.line(format!("{destroy_fn}(handle)"));
        w.line("self.handle = nil");
    });
    w.line("}");
    w.blank();
    w.line("/// Pulls the next element from the producer, or returns `nil` once the");
    w.line("/// stream is exhausted (destroying the underlying iterator).");
    w.line(format!("public func next() -> {elem_swift}? {{"));
    w.indent();
    w.line("guard let handle = handle else { return nil }");
    w.line(format!("var item: {c_var} = {default}"));
    if has_len_slot {
        w.line("var itemLen: Int = 0");
    }
    w.line("var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)");
    if has_len_slot {
        w.line(format!(
            "if {next_fn}(handle, &item, &itemLen, &err) == 0 {{"
        ));
    } else {
        w.line(format!("if {next_fn}(handle, &item, &err) == 0 {{"));
    }
    w.indent();
    w.line("if err.code != 0 {");
    w.indent();
    w.line("let code = err.code");
    w.line("let message = err.message.flatMap { String(cString: $0) } ?? \"\"");
    if throws {
        match &stem {
            Some(stem) => {
                w.line("let payload: [UInt8]? = err.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.payload_len)) }");
                w.line("weaveffi_error_clear(&err)");
                w.line(format!(
                    "self.error = map{stem}(code: code, message: message, payload: payload)"
                ));
            }
            None => {
                w.line("weaveffi_error_clear(&err)");
                w.line(format!(
                    "self.error = {ERROR_BRAND}.error(code: code, message: message)"
                ));
            }
        }
    } else {
        w.line("weaveffi_error_clear(&err)");
        w.line("fatalError(\"\\(code): \\(message)\")");
    }
    w.dedent();
    w.line("}");
    w.line("destroyHandle()");
    w.line("return nil");
    w.dedent();
    w.line("}");

    if is_buffered_elem {
        // Decode the element buffer, then release it (ElemFree::Bytes).
        w.line("let itemBytes = [UInt8](UnsafeBufferPointer(start: item, count: itemLen))");
        w.line("weaveffi_free_bytes(UnsafeMutablePointer(mutating: item), itemLen)");
        w.line("var itemReader = WvReader(bytes: itemBytes)");
        let mut counter = 0usize;
        let v = fresh(&mut counter, "v");
        read_value_stmts(&mut w, inner, &v, "itemReader", ctx, &mut counter);
        w.line("itemReader.finish()");
        w.line(format!("return {v}"));
    } else if is_bytes_elem {
        w.line("let element = Data(bytes: item!, count: itemLen)");
        w.line("weaveffi_free_bytes(UnsafeMutablePointer(mutating: item), itemLen)");
        w.line("return element");
    } else {
        let convert = match inner {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String(cString: item!)".to_string(),
            // An owned interface element is adopted by the wrapper class,
            // whose deinit owes the `_destroy`.
            TypeRef::Interface(name) => {
                format!("{}(ptr: item!)", ctx.ty_name(local_type_name(name)))
            }
            TypeRef::TypedHandle(_) => "UInt64(UInt(bitPattern: item))".to_string(),
            TypeRef::Enum(name) => format!(
                "{}(rawValue: item.rawValue)!",
                ctx.ty_name(local_type_name(name))
            ),
            _ => "item".to_string(),
        };
        w.line(format!("let element = {convert}"));
        if protocol.elem_free == ElemFree::String {
            w.line("weaveffi_free_string(item)");
        }
        w.line("return element");
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests;
