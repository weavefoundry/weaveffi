//! Go (CGo) binding generator for WeaveFFI.
//!
//! Emits a Go module (`go.mod` + package) with CGo bindings over the C
//! ABI exposed by the underlying cdylib. Implements [`LanguageBackend`];
//! the shared driver bridges it into the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{AbiParam, CType, ConstPos};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, walk_modules, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackBinding, EnumBinding, FieldBinding, FnBinding,
    ListenerBinding, ModuleBinding, ParamBinding, RichVariantBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::platform::Platform;
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_prelude, render_trailer, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`GoGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoConfig {
    /// Go module path written to `go.mod` (default `"weaveffi"`).
    pub module_path: Option<String>,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the cgo bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl GoConfig {
    /// Returns the configured Go module path, falling back to `"weaveffi"`.
    pub fn module_path(&self) -> &str {
        self.module_path.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Go backend: emits a CGo package (`weaveffi.go`, `go.mod`, and a README)
/// binding the C ABI exposed by the underlying cdylib.
pub struct GoGenerator;

impl LanguageBackend for GoGenerator {
    type Config = GoConfig;

    fn name(&self) -> &'static str {
        "go"
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
        let dir = out_dir.join("go");
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                dir.join("weaveffi.go"),
                render_go(api, config.prefix(), input_basename),
            ),
            OutputFile::new(
                dir.join("go.mod"),
                render_go_mod(
                    &pkg::resolve(
                        api,
                        config.module_path.as_deref(),
                        config.input_basename.as_deref(),
                    )
                    .name,
                    input_basename,
                ),
            ),
            OutputFile::new(dir.join("README.md"), render_readme(input_basename)),
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
        let dir = out_dir.join("go");
        let input_basename = config.input_basename();
        let prefix = config.prefix();
        let link_name = pkg::resolve(api, None, Some(input_basename)).ident_name();
        let module_path = pkg::resolve(
            api,
            config.module_path.as_deref(),
            config.input_basename.as_deref(),
        )
        .name;

        // Expand the single generate-mode `#cgo LDFLAGS` line into a
        // self-contained, relocatable set: a header include path plus per
        // GOOS/GOARCH library search + rpath directives (all `${SRCDIR}`
        // relative). cgo selects the matching line at build time.
        let original = format!("#cgo LDFLAGS: -l{link_name}\n");
        let mut cgo = String::from("#cgo CFLAGS: -I${SRCDIR}/../c/include\n");
        for nb in &ctx.binaries.binaries {
            let (goos, goarch) = go_build_tags(nb.platform);
            let id = nb.platform.id();
            if nb.platform == Platform::WindowsX64 {
                cgo.push_str(&format!(
                    "#cgo {goos},{goarch} LDFLAGS: -L${{SRCDIR}}/lib/{id}\n"
                ));
            } else {
                cgo.push_str(&format!(
                    "#cgo {goos},{goarch} LDFLAGS: -L${{SRCDIR}}/lib/{id} -Wl,-rpath,${{SRCDIR}}/lib/{id}\n"
                ));
            }
        }
        cgo.push_str(&format!("#cgo LDFLAGS: -l{link_name}\n"));
        let go_src = render_go(api, prefix, input_basename).replace(&original, &cgo);

        let mut files = vec![
            PackagedFile::text(dir.join("weaveffi.go"), go_src),
            PackagedFile::text(
                dir.join("go.mod"),
                render_go_mod(&module_path, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(ctx, input_basename),
            ),
        ];
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

weaveffi_core::impl_generator_via_backend!(GoGenerator);

/// The `(GOOS, GOARCH)` build-constraint tokens for a [`Platform`], used on
/// `#cgo` directive lines.
fn go_build_tags(p: Platform) -> (&'static str, &'static str) {
    match p {
        Platform::MacosArm64 => ("darwin", "arm64"),
        Platform::MacosX64 => ("darwin", "amd64"),
        Platform::LinuxX64 => ("linux", "amd64"),
        Platform::LinuxArm64 => ("linux", "arm64"),
        Platform::WindowsX64 => ("windows", "amd64"),
    }
}

/// README for a packaged Go module that bundles per-platform libraries.
fn render_packaged_readme(ctx: &PackageContext, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# WeaveFFI (Go)

Auto-generated cgo bindings with a prebuilt shared library bundled for each
platform under `lib/<platform>/`. The cgo preamble adds the matching
`${{SRCDIR}}`-relative library search path and rpath per GOOS/GOARCH, so
`go build` links the right library with no manual `CGO_LDFLAGS`.

The C ABI header is expected at `../c/include/` (package the `c` target
alongside Go, for example `weaveffi package --target c,go`).

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}

// ── Type mapping ──

fn go_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "int8".into(),
        TypeRef::I16 => "int16".into(),
        TypeRef::I32 => "int32".into(),
        TypeRef::U8 => "uint8".into(),
        TypeRef::U16 => "uint16".into(),
        TypeRef::U32 => "uint32".into(),
        TypeRef::U64 => "uint64".into(),
        TypeRef::I64 | TypeRef::Handle => "int64".into(),
        TypeRef::F32 => "float32".into(),
        TypeRef::F64 => "float64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "[]byte".into(),
        // Structs, enums, and typed handles surface as bare local Go types; a
        // cross-module typed handle (resolved to e.g. `kv.Store`) must name the
        // local `Store` type rather than the qualified `KvStore`.
        TypeRef::TypedHandle(n) => format!("*{}", local_type_name(n).to_upper_camel_case()),
        TypeRef::Struct(n) => format!("*{}", local_type_name(n).to_upper_camel_case()),
        TypeRef::Enum(n) => local_type_name(n).to_upper_camel_case(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => go_type(inner),
            TypeRef::List(_) | TypeRef::Map(_, _) => go_type(inner),
            TypeRef::Bytes | TypeRef::BorrowedBytes => go_type(inner),
            _ => format!("*{}", go_type(inner)),
        },
        TypeRef::List(inner) | TypeRef::Iterator(inner) => format!("[]{}", go_type(inner)),
        TypeRef::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
    }
}

fn go_zero(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::F32
        | TypeRef::F64 => "0".into(),
        TypeRef::Bool => "false".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "\"\"".into(),
        TypeRef::Enum(_) => "0".into(),
        _ => "nil".into(),
    }
}

fn c_scalar_type(ty: &TypeRef, prefix: &str, module: &str) -> Option<String> {
    match ty {
        TypeRef::I8 => Some("C.int8_t".into()),
        TypeRef::I16 => Some("C.int16_t".into()),
        TypeRef::I32 => Some("C.int32_t".into()),
        TypeRef::U8 => Some("C.uint8_t".into()),
        TypeRef::U16 => Some("C.uint16_t".into()),
        TypeRef::U32 => Some("C.uint32_t".into()),
        TypeRef::U64 => Some("C.uint64_t".into()),
        TypeRef::I64 | TypeRef::Handle => Some("C.int64_t".into()),
        TypeRef::F32 => Some("C.float".into()),
        TypeRef::F64 => Some("C.double".into()),
        TypeRef::Bool => Some("C._Bool".into()),
        TypeRef::Enum(n) => Some(format!("C.{}", c_abi_struct_name(n, module, prefix))),
        _ => None,
    }
}

fn c_scalar_conv(expr: &str, ty: &TypeRef, prefix: &str, module: &str) -> String {
    match ty {
        TypeRef::Bool => format!("boolToC({expr})"),
        _ => {
            if let Some(ct) = c_scalar_type(ty, prefix, module) {
                format!("{ct}({expr})")
            } else {
                expr.to_string()
            }
        }
    }
}

fn go_scalar_conv(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => format!("int8({expr})"),
        TypeRef::I16 => format!("int16({expr})"),
        TypeRef::I32 => format!("int32({expr})"),
        TypeRef::U8 => format!("uint8({expr})"),
        TypeRef::U16 => format!("uint16({expr})"),
        TypeRef::U32 => format!("uint32({expr})"),
        TypeRef::U64 => format!("uint64({expr})"),
        TypeRef::I64 | TypeRef::Handle => format!("int64({expr})"),
        TypeRef::F32 => format!("float32({expr})"),
        TypeRef::F64 => format!("float64({expr})"),
        TypeRef::Bool => format!("cToBool({expr})"),
        TypeRef::Enum(n) => format!("{}({expr})", local_type_name(n).to_upper_camel_case()),
        _ => expr.to_string(),
    }
}

fn c_opaque_type(ty: &TypeRef, prefix: &str, module: &str) -> String {
    match ty {
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => c_abi_struct_name(n, module, prefix),
        _ => String::new(),
    }
}

// ── Import scanning ──

fn param_uses_unsafe(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => true,
        TypeRef::Bytes | TypeRef::BorrowedBytes => true,
        TypeRef::List(_) | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => param_uses_unsafe(inner),
        _ => false,
    }
}

fn return_uses_unsafe(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => true,
        TypeRef::List(_) | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => return_uses_unsafe(inner),
        _ => false,
    }
}

fn type_has_bool(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Bool => true,
        TypeRef::Optional(inner) | TypeRef::List(inner) => type_has_bool(inner),
        _ => false,
    }
}

/// Imports the generated file needs: (`fmt`, `unsafe`, bool helpers, `sync`).
fn scan_imports(api: &Api) -> (bool, bool, bool, bool) {
    let has_funcs = walk_modules(&api.modules).any(|m| !m.functions.is_empty());
    // A builder's `Build` calls the C `create` symbol and returns `(*T, error)`,
    // so it pulls in `fmt` (error formatting) just like a fallible function.
    let has_builder = walk_modules(&api.modules).any(|m| m.structs.iter().any(|s| s.builder));
    // A rich (algebraic) enum emits a `New{Variant}` constructor per variant
    // that returns `(*T, error)` and formats failures with `fmt`, exactly like
    // a builder's `Build`.
    let has_rich_enum = walk_modules(&api.modules).any(|m| m.enums.iter().any(|e| e.is_rich()));
    let has_async = walk_modules(&api.modules).any(|m| m.functions.iter().any(|f| f.r#async));
    let has_listeners = walk_modules(&api.modules).any(|m| !m.listeners.is_empty());

    let needs_fmt = has_funcs || has_builder || has_rich_enum;

    // Async launchers and listener registration thread the registry id through
    // the C `void* context`, which always stages through unsafe.Pointer.
    let needs_unsafe = has_async
        || has_listeners
        || walk_modules(&api.modules).any(|m| {
            m.functions.iter().any(|f| {
                f.params.iter().any(|p| param_uses_unsafe(&p.ty))
                    || f.returns.as_ref().is_some_and(return_uses_unsafe)
            }) || m.structs.iter().any(|s| {
                // Getters can materialize bytes/list/map; a builder additionally
                // marshals every field *in* (strings stage through unsafe.Pointer).
                s.fields.iter().any(|fld| return_uses_unsafe(&fld.ty))
                    || (s.builder && s.fields.iter().any(|fld| param_uses_unsafe(&fld.ty)))
            }) || m.enums.iter().any(|e| {
                // A rich enum's per-variant field getters materialize
                // bytes/list/map, and its constructors marshal those fields *in*
                // (strings stage through unsafe.Pointer), just like a struct.
                e.is_rich()
                    && e.variants.iter().any(|v| {
                        v.fields
                            .iter()
                            .any(|fld| return_uses_unsafe(&fld.ty) || param_uses_unsafe(&fld.ty))
                    })
            })
        });

    let needs_bool = walk_modules(&api.modules).any(|m| {
        m.functions.iter().any(|f| {
            f.params.iter().any(|p| type_has_bool(&p.ty))
                || f.returns.as_ref().is_some_and(type_has_bool)
        }) || m
            .structs
            .iter()
            .any(|s| s.fields.iter().any(|fld| type_has_bool(&fld.ty)))
            || m.enums.iter().any(|e| {
                e.is_rich()
                    && e.variants
                        .iter()
                        .any(|v| v.fields.iter().any(|fld| type_has_bool(&fld.ty)))
            })
            || m.callbacks
                .iter()
                .any(|c| c.params.iter().any(|p| type_has_bool(&p.ty)))
    });

    let needs_sync = has_async || has_listeners;

    (needs_fmt, needs_unsafe, needs_bool, needs_sync)
}

// ── Packaging scaffold ──

fn render_go_mod(module_path: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let trailer = render_trailer(CommentStyle::DoubleSlash, "go.mod");
    format!("{prelude}module {module_path}\n\ngo 1.21\n\n{trailer}")
}

fn render_readme(input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    format!(
        r#"{prelude}# WeaveFFI Go Bindings

Auto-generated Go bindings using CGo.

## Prerequisites

- Go >= 1.21
- A C compiler (gcc or clang) accessible to CGo
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`,
  or `weaveffi.dll`) and the C header (`weaveffi.h`)

## Build

1. Place `libweaveffi.so` (or the platform-specific equivalent) and
   `weaveffi.h` where the linker and CGo can find them. For example,
   install them into `/usr/local/lib` and `/usr/local/include`, or set
   `CGO_LDFLAGS` and `CGO_CFLAGS`:

```sh
export CGO_CFLAGS="-I/path/to/headers"
export CGO_LDFLAGS="-L/path/to/lib -lweaveffi"
```

2. Build or run your Go project that imports this module:

```sh
go build ./...
```

## How It Works

The generated `weaveffi.go` file uses a CGo preamble to `#include "weaveffi.h"`
and link against `-lweaveffi`. Each API function is exposed as an idiomatic Go
function that marshals arguments to C types, calls the C ABI function, and
converts the result back to Go types. Errors are returned as Go `error` values.

{trailer}"#
    )
}

// ── Top-level rendering ──

/// Emits a Go `// ...` doc comment at `indent`. If `symbol` is provided, the
/// first non-empty line is prefixed with the symbol name to follow Go's doc
/// convention. Subsequent lines are emitted verbatim with `// `.
///
/// Without a symbol, this delegates to the shared
/// [`weaveffi_core::codegen::common::emit_doc`] helper using
/// [`DocCommentStyle::DoubleSlash`]. The symbol-prefix flavour stays
/// generator-local because godoc's first-line convention is unique to Go.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str, symbol: Option<&str>) {
    let Some(symbol) = symbol else {
        common_emit_doc(out, doc, indent, DocCommentStyle::DoubleSlash);
        return;
    };
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    let mut lines = doc.lines();
    if let Some(first) = lines.next() {
        out.push_str(indent);
        let lower = first
            .chars()
            .next()
            .map(|c| c.is_lowercase())
            .unwrap_or(false);
        if lower {
            out.push_str(&format!("// {symbol} {}\n", first));
        } else {
            out.push_str(&format!("// {symbol}: {}\n", first));
        }
    }
    for line in lines {
        out.push_str(indent);
        if line.is_empty() {
            out.push_str("//\n");
        } else {
            out.push_str("// ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Emits a Go function doc comment with continuation lines for any documented
/// parameters. Skips entirely when there is nothing to emit.
fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
    symbol: &str,
) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&ParamBinding> = params
        .iter()
        .filter(|p| {
            p.doc
                .as_ref()
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if trimmed_doc.is_none() && documented_params.is_empty() {
        return;
    }
    if let Some(d) = trimmed_doc {
        emit_doc(out, &Some(d.to_string()), indent, Some(symbol));
    } else {
        out.push_str(indent);
        out.push_str(&format!("// {symbol} ...\n"));
    }
    if !documented_params.is_empty() {
        out.push_str(indent);
        out.push_str("//\n");
        out.push_str(indent);
        out.push_str("// Parameters:\n");
        for p in documented_params {
            let pdoc = p.doc.as_ref().unwrap().trim();
            let mut lines = pdoc.lines();
            let first = lines.next().unwrap_or("");
            out.push_str(indent);
            out.push_str(&format!("//   - {}: {}\n", p.name, first));
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str("//\n");
                } else {
                    out.push_str("//     ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
}

fn render_go(api: &Api, prefix: &str, input_basename: &str) -> String {
    let model = BindingModel::build(api, prefix);
    let (needs_fmt, needs_unsafe, needs_bool, needs_sync) = scan_imports(api);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);

    // The Go package clause and the linked library name follow the resolved
    // package identity (e.g. `package kvstore` / `-lkvstore`) rather than the
    // `weaveffi` brand, so the bindings link the shared library the producer
    // emits for this package. The C header keeps the ABI-prefix name.
    let resolved = pkg::resolve(api, None, Some(input_basename));
    let go_pkg = resolved.ident_name();
    let link_name = resolved.ident_name();

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    let has_async = model
        .modules
        .iter()
        .any(|m| m.functions.iter().any(|f| f.is_async));

    out.push_str(&format!("package {go_pkg}\n\n"));

    out.push_str("/*\n");
    out.push_str(&format!("#cgo LDFLAGS: -l{link_name}\n"));
    out.push_str(&format!("#include \"{prefix}.h\"\n"));
    out.push_str("#include <stdlib.h>\n");
    // Forward declarations for the //export trampolines below. These must
    // mirror the prototypes cgo emits into _cgo_export.h (const-free), and the
    // preamble of a file using //export may only contain declarations.
    for decl in collect_trampoline_externs(&model, prefix) {
        out.push_str(&decl);
        out.push('\n');
    }
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    if needs_fmt || needs_unsafe || needs_sync {
        out.push_str("\nimport (\n");
        if needs_fmt {
            out.push_str("\t\"fmt\"\n");
        }
        if needs_sync {
            out.push_str("\t\"sync\"\n");
        }
        if needs_unsafe {
            out.push_str("\t\"unsafe\"\n");
        }
        out.push_str(")\n");
    }
    out.push('\n');

    if needs_bool {
        // cgo models C `_Bool` as a distinct Go type whose underlying kind is
        // bool, so convert with the type itself rather than integer literals.
        out.push_str("func boolToC(b bool) C._Bool {\n");
        out.push_str("\treturn C._Bool(b)\n");
        out.push_str("}\n\n");
        out.push_str("func cToBool(b C._Bool) bool {\n");
        out.push_str("\treturn bool(b)\n");
        out.push_str("}\n\n");
    }

    if has_async || has_listeners {
        render_callback_registry(&mut out, has_listeners);
    }

    for m in &model.modules {
        for e in &m.enums {
            // A plain C-style enum becomes an `int32` + constants; a rich
            // (algebraic) enum becomes an opaque-object wrapper. Each renderer
            // skips the other kind, mirroring the C++ backend.
            render_enum(&mut out, e);
            render_rich_enum(&mut out, prefix, &m.path, e);
        }
        for s in &m.structs {
            render_struct(&mut out, prefix, &m.path, s);
            if s.builder.is_some() {
                render_go_builder(&mut out, prefix, &m.path, s);
            }
        }
        for cb in &m.callbacks {
            render_callback_trampoline(&mut out, prefix, &m.path, cb);
        }
        for l in &m.listeners {
            render_listener_api(&mut out, m, l);
        }
        for f in &m.functions {
            if let CallShape::Async(ab) = &f.shape {
                render_async_function(&mut out, prefix, &m.path, f, ab);
            } else {
                render_function(&mut out, prefix, &m.path, f);
            }
        }
    }

    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi.go"));
    out
}

// ── Callbacks, listeners, and async support ──

/// Go formal type for one C ABI slot in a trampoline signature.
fn cgo_slot_type(ct: &CType, prefix: &str) -> String {
    match ct {
        CType::Int8 => "C.int8_t".into(),
        CType::Int16 => "C.int16_t".into(),
        CType::Int32 => "C.int32_t".into(),
        CType::Uint8 => "C.uint8_t".into(),
        CType::Uint16 => "C.uint16_t".into(),
        CType::Uint32 => "C.uint32_t".into(),
        CType::Int64 => "C.int64_t".into(),
        CType::Uint64 => "C.uint64_t".into(),
        CType::Float => "C.float".into(),
        CType::Double => "C.double".into(),
        CType::Bool => "C._Bool".into(),
        CType::Size => "C.size_t".into(),
        CType::Char => "C.char".into(),
        CType::Handle => format!("C.{prefix}_handle_t"),
        CType::CancelToken => format!("C.{prefix}_cancel_token"),
        CType::Error => format!("C.{prefix}_error"),
        CType::Enum { module, name } | CType::StructTag { module, name } => {
            format!("C.{prefix}_{module}_{name}")
        }
        CType::Named(core) => format!("C.{prefix}_{core}"),
        CType::Ptr { pointee, .. } => {
            if **pointee == CType::Void {
                "unsafe.Pointer".into()
            } else {
                format!("*{}", cgo_slot_type(pointee, prefix))
            }
        }
        CType::Void => unreachable!("void only appears behind a pointer"),
    }
}

/// `ct` with every `const` qualifier dropped, matching the const-free
/// prototypes cgo writes into `_cgo_export.h` for exported Go functions.
fn strip_const(ct: &CType) -> CType {
    match ct {
        CType::Ptr { pointee, .. } => CType::Ptr {
            konst: ConstPos::None,
            pointee: Box::new(strip_const(pointee)),
        },
        other => other.clone(),
    }
}

/// The C name of the exported Go trampoline for a callback/async typedef.
fn trampoline_name(c_type_name: &str) -> String {
    format!("goWv_{c_type_name}")
}

/// The preamble `extern` declaration for one exported trampoline.
fn extern_decl(c_type_name: &str, params: &[AbiParam], prefix: &str) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| format!("{} {}", strip_const(&p.ty).render_c(prefix), p.name))
        .collect();
    format!(
        "extern void {}({});",
        trampoline_name(c_type_name),
        args.join(", ")
    )
}

/// Every `extern` declaration the preamble needs: one per module callback
/// (shared by all listeners firing it) and one per async completion callback.
fn collect_trampoline_externs(model: &BindingModel, prefix: &str) -> Vec<String> {
    let mut decls = Vec::new();
    for m in &model.modules {
        for cb in &m.callbacks {
            decls.push(extern_decl(&cb.c_fn_type, &cb.abi_params, prefix));
        }
        for f in &m.functions {
            if let CallShape::Async(ab) = &f.shape {
                decls.push(extern_decl(&ab.callback_type, &ab.callback_params, prefix));
            }
        }
    }
    decls
}

/// The registry mapping opaque context ids to Go callbacks/channels. Only the
/// integer id (never a Go pointer) crosses the C boundary as `void*`, so the
/// GC stays unaware of C-held references and trampolines recover the Go value
/// from the map.
fn render_callback_registry(out: &mut String, has_listeners: bool) {
    let mut w = CodeWriter::tabs();
    w.block("var (", ")", |w| {
        w.line("wvCallbackMu  sync.Mutex");
        w.line("wvCallbackSeq uint64");
        w.line("wvCallbacks   = map[uint64]interface{}{}");
        if has_listeners {
            w.line("// Subscription id -> registry id, so unregister can release the Go callback.");
            w.line("wvListenerCtx = map[uint64]uint64{}");
        }
    });
    w.blank();

    w.block("func wvCallbackStore(v interface{}) uint64 {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("wvCallbackSeq++");
        w.line("wvCallbacks[wvCallbackSeq] = v");
        w.line("return wvCallbackSeq");
    });
    w.blank();

    w.block("func wvCallbackLoad(id uint64) interface{} {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("return wvCallbacks[id]");
    });
    w.blank();

    w.block("func wvCallbackTake(id uint64) interface{} {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("v := wvCallbacks[id]");
        w.line("delete(wvCallbacks, id)");
        w.line("return v");
    });
    w.blank();

    w.block("func wvCallbackDelete(id uint64) {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("delete(wvCallbacks, id)");
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The Go signature of the user-facing callback for a module callback decl,
/// e.g. `func(key string)`.
fn go_callback_sig(cb: &CallbackBinding) -> String {
    let params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();
    format!("func({})", params.join(", "))
}

/// Emit statements converting one callback parameter's C slots into a Go
/// value bound to `arg{idx}`, returning that local's name.
fn emit_cb_param_arg(
    out: &mut String,
    idx: usize,
    p: &ParamBinding,
    prefix: &str,
    module: &str,
) -> String {
    let arg = format!("arg{idx}");
    let n = &p.abi[0].name;
    let mut w = CodeWriter::tabs().with_depth(1);
    match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::F32
        | TypeRef::F64 => {
            w.line(format!("{arg} := {}", go_scalar_conv(n, &p.ty)));
        }
        TypeRef::Bool => {
            w.line(format!("{arg} := cToBool({n})"));
        }
        TypeRef::Enum(_) => {
            w.line(format!("{arg} := {}", go_scalar_conv(n, &p.ty)));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{arg} := \"\""));
            w.block(format!("if {n} != nil {{"), "}", |w| {
                w.line(format!("{arg} = C.GoString({n})"));
            });
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("var {arg} []byte"));
            w.block(format!("if {n} != nil {{"), "}", |w| {
                w.line(format!(
                    "{arg} = C.GoBytes(unsafe.Pointer({n}), C.int({}_len))",
                    p.name
                ));
            });
        }
        // Opaque pointers are borrowed for the duration of the callback; the
        // wrapper must not be Closed by the consumer.
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let g = local_type_name(name).to_upper_camel_case();
            w.line(format!("{arg} := &{g}{{ptr: {n}}}"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("var {arg} *string"));
                w.block(format!("if {n} != nil {{"), "}", |w| {
                    w.line(format!("v{idx} := C.GoString({n})"));
                    w.line(format!("{arg} = &v{idx}"));
                });
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line(format!("var {arg} []byte"));
                w.block(format!("if {n} != nil {{"), "}", |w| {
                    w.line(format!(
                        "{arg} = C.GoBytes(unsafe.Pointer({n}), C.int({}_len))",
                        p.name
                    ));
                });
            }
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let g = local_type_name(name).to_upper_camel_case();
                w.line(format!("var {arg} *{g}"));
                w.block(format!("if {n} != nil {{"), "}", |w| {
                    w.line(format!("{arg} = &{g}{{ptr: {n}}}"));
                });
            }
            TypeRef::Bool => {
                w.line(format!("var {arg} *bool"));
                w.block(format!("if {n} != nil {{"), "}", |w| {
                    w.line(format!("v{idx} := cToBool(*{n})"));
                    w.line(format!("{arg} = &v{idx}"));
                });
            }
            _ => {
                let gt = go_type(inner);
                w.line(format!("var {arg} *{gt}"));
                w.block(format!("if {n} != nil {{"), "}", |w| {
                    w.line(format!("v{idx} := {gt}(*{n})"));
                    w.line(format!("{arg} = &v{idx}"));
                });
            }
        },
        TypeRef::List(inner) => {
            w.line(format!("count{idx} := int({}_len)", p.name));
            let mut body = String::new();
            decode_list(
                &mut body,
                &arg,
                inner,
                n,
                &format!("count{idx}"),
                prefix,
                module,
            );
            w.raw(body);
        }
        TypeRef::Map(k, v) => {
            w.line(format!("count{idx} := int({}_len)", p.name));
            let mut body = String::new();
            decode_map(
                &mut body,
                &arg,
                k,
                v,
                &format!("{}_keys", p.name),
                &format!("{}_values", p.name),
                &format!("count{idx}"),
                prefix,
                module,
            );
            w.raw(body);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
    out.push_str(&w.finish());
    arg
}

/// One exported trampoline per module callback declaration; every listener
/// firing this callback shares it, with the registry id in `context` selecting
/// the Go callback.
fn render_callback_trampoline(out: &mut String, prefix: &str, module: &str, cb: &CallbackBinding) {
    let tramp = trampoline_name(&cb.c_fn_type);
    let formals: Vec<String> = cb
        .abi_params
        .iter()
        .map(|s| format!("{} {}", s.name, cgo_slot_type(&s.ty, prefix)))
        .collect();

    let mut w = CodeWriter::tabs();
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            w.line("v := wvCallbackLoad(uint64(uintptr(context)))");
            w.block("if v == nil {", "}", |w| {
                w.line("return");
            });
            w.line(format!("cb := v.({})", go_callback_sig(cb)));
            let mut args = Vec::new();
            for (idx, p) in cb.params.iter().enumerate() {
                let mut body = String::new();
                args.push(emit_cb_param_arg(&mut body, idx, p, prefix, module));
                w.raw(body);
            }
            w.line(format!("cb({})", args.join(", ")));
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The register/unregister wrapper pair for one listener.
fn render_listener_api(out: &mut String, m: &ModuleBinding, l: &ListenerBinding) {
    let Some(cb) = m.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_go = format!("{}_register_{}", m.path, l.name).to_upper_camel_case();
    let unregister_go = format!("{}_unregister_{}", m.path, l.name).to_upper_camel_case();
    let tramp = trampoline_name(&cb.c_fn_type);

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &l.doc, "", Some(&register_go));
    w.raw(d);
    w.line(format!("// Returns a subscription id for {unregister_go}."));
    w.block(
        format!("func {register_go}(callback {}) uint64 {{", go_callback_sig(cb)),
        "}",
        |w| {
            w.line("ctxID := wvCallbackStore(callback)");
            w.line(format!(
                "id := uint64(C.{}(C.{}(unsafe.Pointer(C.{tramp})), unsafe.Pointer(uintptr(ctxID))))",
                l.register_symbol, cb.c_fn_type
            ));
            w.line("wvCallbackMu.Lock()");
            w.line("wvListenerCtx[id] = ctxID");
            w.line("wvCallbackMu.Unlock()");
            w.line("return id");
        },
    );
    w.blank();

    w.line(format!(
        "// {unregister_go} unregisters a listener previously registered with {register_go}."
    ));
    w.block(format!("func {unregister_go}(id uint64) {{"), "}", |w| {
        w.line(format!("C.{}(C.uint64_t(id))", l.unregister_symbol));
        w.line("wvCallbackMu.Lock()");
        w.line("ctxID, ok := wvListenerCtx[id]");
        w.line("delete(wvListenerCtx, id)");
        w.line("wvCallbackMu.Unlock()");
        w.block("if ok {", "}", |w| {
            w.line("wvCallbackDelete(ctxID)");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The per-async-function outcome payload type name.
fn async_outcome_type(module: &str, f: &FnBinding) -> String {
    format!(
        "wvOutcome{}",
        format!("{}_{}", module, f.name).to_upper_camel_case()
    )
}

/// Send the converted async result over the outcome channel. Runs inside the
/// completion trampoline after the error path has been handled.
fn emit_async_result_send(
    out: &mut String,
    ret: &Option<TypeRef>,
    outcome: &str,
    prefix: &str,
    module: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    let Some(ty) = ret else {
        w.line(format!("ch <- {outcome}{{}}"));
        out.push_str(&w.finish());
        return;
    };
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::F32
        | TypeRef::F64 => {
            w.line(format!(
                "ch <- {outcome}{{val: {}}}",
                go_scalar_conv("result", ty)
            ));
        }
        TypeRef::Bool => {
            w.line(format!("ch <- {outcome}{{val: cToBool(result)}}"));
        }
        TypeRef::Enum(_) => {
            w.line(format!(
                "ch <- {outcome}{{val: {}}}",
                go_scalar_conv("result", ty)
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("val := \"\"");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoString(result)");
                w.line("C.weaveffi_free_string(result)");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("var val []byte");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoBytes(unsafe.Pointer(result), C.int(result_len))");
                w.line("C.weaveffi_free_bytes(result, result_len)");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("ch <- {outcome}{{val: &{g}{{ptr: result}}}}"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line("var val *string");
                w.block("if result != nil {", "}", |w| {
                    w.line("v := C.GoString(result)");
                    w.line("C.weaveffi_free_string(result)");
                    w.line("val = &v");
                });
                w.line(format!("ch <- {outcome}{{val: val}}"));
            }
            TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
                let g = local_type_name(n).to_upper_camel_case();
                w.line(format!("var val *{g}"));
                w.block("if result != nil {", "}", |w| {
                    w.line(format!("val = &{g}{{ptr: result}}"));
                });
                w.line(format!("ch <- {outcome}{{val: val}}"));
            }
            TypeRef::Bool => {
                w.line("var val *bool");
                w.block("if result != nil {", "}", |w| {
                    w.line("v := cToBool(*result)");
                    w.line("val = &v");
                });
                w.line(format!("ch <- {outcome}{{val: val}}"));
            }
            _ => {
                let gt = go_type(inner);
                w.line(format!("var val *{gt}"));
                w.block("if result != nil {", "}", |w| {
                    w.line(format!("v := {gt}(*result)"));
                    w.line("val = &v");
                });
                w.line(format!("ch <- {outcome}{{val: val}}"));
            }
        },
        TypeRef::List(inner) => {
            w.line("count := int(result_len)");
            let mut body = String::new();
            decode_list(&mut body, "val", inner, "result", "count", prefix, module);
            w.raw(body);
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        TypeRef::Map(k, v) => {
            w.line("count := int(result_len)");
            let mut body = String::new();
            decode_map(
                &mut body,
                "val",
                k,
                v,
                "result_keys",
                "result_values",
                "count",
                prefix,
                module,
            );
            w.raw(body);
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        TypeRef::Iterator(_) => unreachable!("async iterator returns are rejected upstream"),
    }
    out.push_str(&w.finish());
}

/// An async function: a blocking Go wrapper that launches the C call with a
/// completion trampoline and waits on a buffered channel, plus the outcome
/// type and the exported trampoline itself.
fn render_async_function(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    ab: &AsyncBinding,
) {
    let go_name = format!("{}_{}", module, f.name).to_upper_camel_case();
    let outcome = async_outcome_type(module, f);
    let tramp = trampoline_name(&ab.callback_type);

    let mut w = CodeWriter::tabs();

    // Outcome payload: the converted result (if any) or the producer error.
    w.block(format!("type {outcome} struct {{"), "}", |w| {
        if let Some(ret) = &f.ret {
            w.line(format!("val {}", go_type(ret)));
        }
        w.line("err error");
    });
    w.blank();

    // The exported completion trampoline.
    let formals: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", s.name, cgo_slot_type(&s.ty, prefix)))
        .collect();
    let mut tramp_body = String::new();
    emit_async_result_send(&mut tramp_body, &f.ret, &outcome, prefix, module);
    w.line(format!("//export {tramp}"));
    w.block(format!("func {tramp}({}) {{", formals.join(", ")), "}", |w| {
        w.line("v := wvCallbackTake(uint64(uintptr(context)))");
        w.block("if v == nil {", "}", |w| {
            w.line("return");
        });
        w.line(format!("ch := v.(chan {outcome})"));
        w.block("if err != nil && err.code != 0 {", "}", |w| {
            w.line("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(err.message), int(err.code))");
            w.line("C.weaveffi_error_clear(err)");
            w.line(format!("ch <- {outcome}{{err: goErr}}"));
            w.line("return");
        });
        w.raw(tramp_body.as_str());
    });
    w.blank();

    // The blocking wrapper. Cancellation tokens are not surfaced (NULL).
    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();
    let ret_sig = match &f.ret {
        Some(ret) => format!("({}, error)", go_type(ret)),
        None => "error".into(),
    };
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", &go_name);
    w.raw(doc);
    w.line("// Blocks until the async producer completes.");
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    for p in &f.params {
        emit_param(
            &mut pre,
            &mut c_args,
            &p.name.to_lower_camel_case(),
            &p.ty,
            prefix,
            module,
        );
    }
    if f.cancellable {
        c_args.push("nil".into());
    }
    c_args.push(format!("C.{}(unsafe.Pointer(C.{tramp}))", ab.callback_type));
    c_args.push("unsafe.Pointer(uintptr(ctxID))".into());
    let launch_args = c_args.join(", ");

    w.block(
        format!("func {go_name}({}) {ret_sig} {{", go_params.join(", ")),
        "}",
        |w| {
            w.line(format!("ch := make(chan {outcome}, 1)"));
            w.line("ctxID := wvCallbackStore(ch)");
            w.raw(pre.as_str());
            w.line(format!("C.{}({})", ab.launch.symbol, launch_args));
            w.line("outcome := <-ch");
            if let Some(ret) = &f.ret {
                w.block("if outcome.err != nil {", "}", |w| {
                    w.line(format!("return {}, outcome.err", go_zero(ret)));
                });
                w.line("return outcome.val, nil");
            } else {
                w.line("return outcome.err");
            }
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Enums ──

fn render_enum(out: &mut String, e: &EnumBinding) {
    // Rich (algebraic) enums cross the ABI as opaque objects and are rendered
    // as wrappers by `render_rich_enum`; only plain C-style enums are int32s.
    if e.is_rich() {
        return;
    }
    let name = e.name.to_upper_camel_case();
    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "", Some(&name));
    w.raw(d);
    w.line(format!("type {name} int32"));
    w.blank();
    w.block("const (", ")", |w| {
        for v in &e.variants {
            let vname = format!("{name}{}", v.name.to_upper_camel_case());
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "\t", Some(&vname));
            w.raw(vd);
            w.line(format!("{vname} {name} = {}", v.value));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an opaque-object wrapper, mirroring the Go
/// struct wrapper ([`render_struct`]): a value type owning the `*C.{tag}` handle
/// freed by an explicit `Close`, an `int32` discriminant read by `Tag()` plus
/// exported per-variant tag constants (reusing the plain-enum const style), one
/// `New{Enum}{Variant}` constructor per variant calling `{tag}_{V}_new`, and
/// per-variant field accessors (`{Variant}{Field}()`) reusing the struct getter
/// marshalling. Because a rich enum resolves to `TypeRef::Struct`, the existing
/// function/param/return machinery handles it as a value unchanged.
///
/// A plain C-style enum is skipped here (it is handled by [`render_enum`]).
fn render_rich_enum(out: &mut String, prefix: &str, module: &str, e: &EnumBinding) {
    let Some(rich) = &e.rich else {
        return;
    };
    let name = e.name.to_upper_camel_case();
    let c_tag = &e.c_tag;

    let mut w = CodeWriter::tabs();
    // Opaque-object value type owning the C handle (identical to a struct).
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line(format!("ptr *C.{c_tag}"));
    });
    w.blank();

    // Exported discriminant constants in the plain-enum const style. The wrapper
    // type name is taken by the struct above, so these are typed `int32` to
    // match what `Tag` returns (`shape.Tag() == ShapeCircle`).
    w.block("const (", ")", |w| {
        for v in &e.variants {
            let vname = format!("{name}{}", v.name.to_upper_camel_case());
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "\t", Some(&vname));
            w.raw(vd);
            w.line(format!("{vname} int32 = {}", v.value));
        }
    });
    w.blank();

    // Tag reader: the active variant's discriminant.
    w.block(format!("func (s *{name}) Tag() int32 {{"), "}", |w| {
        w.line(format!("return int32(C.{}(s.ptr))", rich.tag_symbol));
    });
    w.blank();

    // One constructor per variant, calling `{tag}_{V}_new`.
    for v in &rich.variants {
        let mut c = String::new();
        render_rich_enum_ctor(&mut c, prefix, module, &name, v);
        w.raw(c);
    }

    // Per-variant field accessors, namespaced by variant to avoid collisions
    // between same-named fields. Reuse `render_getter` so the marshalling is
    // identical to a struct getter; the synthesized `{variant}_{field}` name
    // lowers to a `{Variant}{Field}` method (e.g. `CircleRadius`).
    for v in &rich.variants {
        for f in &v.fields {
            let mut nf = f.clone();
            nf.name = format!("{}_{}", v.name, f.name);
            let mut g = String::new();
            render_getter(&mut g, prefix, module, &name, &nf);
            w.raw(g);
        }
    }

    // Cleanup: identical contract to a struct wrapper's `Close`.
    w.block(format!("func (s *{name}) Close() {{"), "}", |w| {
        w.block("if s.ptr != nil {", "}", |w| {
            w.line(format!("C.{}(s.ptr)", rich.destroy_symbol));
            w.line("s.ptr = nil");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// One rich-enum variant constructor: `New{Enum}{Variant}(<fields>)
/// (*{Enum}, error)`. Each field is marshaled with the same lowering used for a
/// function parameter / struct-builder field, then `{tag}_{V}_new` is called and
/// its `out_err` checked with the shared fallible-call convention. A unit
/// variant takes no parameters (only `out_err`).
fn render_rich_enum_ctor(
    out: &mut String,
    prefix: &str,
    module: &str,
    enum_name: &str,
    v: &RichVariantBinding,
) {
    let ctor = format!("New{enum_name}{}", v.name.to_upper_camel_case());
    let go_params: Vec<String> = v
        .fields
        .iter()
        .map(|f| format!("{} {}", f.name.to_lower_camel_case(), go_type(&f.ty)))
        .collect();

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &v.doc, "", Some(&ctor));
    w.raw(d);

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    for f in &v.fields {
        emit_param(
            &mut pre,
            &mut c_args,
            &f.name.to_lower_camel_case(),
            &f.ty,
            prefix,
            module,
        );
    }
    pre.push_str("\tvar cErr C.weaveffi_error\n");
    c_args.push("&cErr".into());

    w.block(
        format!("func {ctor}({}) (*{enum_name}, error) {{", go_params.join(", ")),
        "}",
        |w| {
            w.raw(&pre);
            w.line(format!(
                "result := C.{}({})",
                v.create.symbol,
                c_args.join(", ")
            ));
            w.block("if cErr.code != 0 {", "}", |w| {
                w.line("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))");
                w.line("C.weaveffi_error_clear(&cErr)");
                w.line("return nil, goErr");
            });
            w.line(format!("return &{enum_name}{{ptr: result}}, nil"));
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Structs ──

fn render_struct(out: &mut String, prefix: &str, module: &str, s: &StructBinding) {
    let name = s.name.to_upper_camel_case();
    // The opaque C tag and destroy symbol are precomputed in the shared model.
    let c_tag = &s.c_tag;

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line(format!("ptr *C.{c_tag}"));
    });
    w.blank();

    for field in &s.fields {
        let mut g = String::new();
        render_getter(&mut g, prefix, module, &name, field);
        w.raw(g);
    }

    w.block(format!("func (s *{name}) Close() {{"), "}", |w| {
        w.block("if s.ptr != nil {", "}", |w| {
            w.line(format!("C.{}(s.ptr)", s.destroy_symbol));
            w.line("s.ptr = nil");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_go_builder(out: &mut String, prefix: &str, module: &str, s: &StructBinding) {
    let name = s.name.to_upper_camel_case();
    let builder_name = format!("{name}Builder");
    // Typed fields (one per struct field) so `Build` can marshal each value into
    // the C `create` call with the same lowering used for function parameters.
    // Optionals/lists/maps default to nil (the C side reads that as "unset").
    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "", Some(&builder_name));
    w.raw(d);
    w.block(format!("type {name}Builder struct {{"), "}", |w| {
        for field in &s.fields {
            let fld = field.name.to_lower_camel_case();
            w.line(format!("{fld} {}", go_type(&field.ty)));
        }
    });
    w.blank();
    w.block(
        format!("func New{name}Builder() *{name}Builder {{"),
        "}",
        |w| {
            w.line(format!("return &{name}Builder{{}}"));
        },
    );
    w.blank();

    for field in &s.fields {
        let method = field.name.to_upper_camel_case();
        let fld = field.name.to_lower_camel_case();
        let gt = go_type(&field.ty);
        let with_name = format!("With{method}");
        let mut fd = String::new();
        emit_doc(&mut fd, &field.doc, "", Some(&with_name));
        w.raw(fd);
        w.block(
            format!("func (b *{name}Builder) With{method}(value {gt}) *{name}Builder {{"),
            "}",
            |w| {
                w.line(format!("b.{fld} = value"));
                w.line("return b");
            },
        );
        w.blank();
    }

    // Build: marshal every field into the struct's `create` call.
    let mut bd = String::new();
    emit_doc(&mut bd, &None, "", Some("Build"));
    w.raw(bd);
    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    for field in &s.fields {
        let fld = field.name.to_lower_camel_case();
        pre.push_str(&format!("\t{fld} := b.{fld}\n"));
        emit_param(&mut pre, &mut c_args, &fld, &field.ty, prefix, module);
    }
    pre.push_str("\tvar cErr C.weaveffi_error\n");
    c_args.push("&cErr".into());
    w.block(
        format!("func (b *{name}Builder) Build() (*{name}, error) {{"),
        "}",
        |w| {
            w.raw(&pre);
            w.line(format!(
                "result := C.{}({})",
                s.create.symbol,
                c_args.join(", ")
            ));
            w.block("if cErr.code != 0 {", "}", |w| {
                w.line("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))");
                w.line("C.weaveffi_error_clear(&cErr)");
                w.line("return nil, goErr");
            });
            w.line(format!("return &{name}{{ptr: result}}, nil"));
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

fn render_getter(
    out: &mut String,
    prefix: &str,
    module: &str,
    go_struct: &str,
    field: &FieldBinding,
) {
    let method = field.name.to_upper_camel_case();
    let ret = go_type(&field.ty);
    let getter = format!("C.{}", field.getter_symbol);

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &field.doc, "", Some(&method));
    w.raw(d);
    w.block(
        format!("func (s *{go_struct}) {method}() {ret} {{"),
        "}",
        |w| match &field.ty {
            TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => {
                let conv = go_scalar_conv(&format!("{getter}(s.ptr)"), &field.ty);
                w.line(format!("return {conv}"));
            }
            TypeRef::Bool => {
                w.line(format!("return cToBool({getter}(s.ptr))"));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("return C.GoString({getter}(s.ptr))"));
            }
            TypeRef::Enum(_) => {
                w.line(format!("return {ret}({getter}(s.ptr))"));
            }
            TypeRef::TypedHandle(n) => {
                let inner = local_type_name(n).to_upper_camel_case();
                w.line(format!("return &{inner}{{ptr: {getter}(s.ptr)}}"));
            }
            TypeRef::Struct(n) => {
                let inner = local_type_name(n).to_upper_camel_case();
                w.line(format!("return &{inner}{{ptr: {getter}(s.ptr)}}"));
            }
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line(format!("cStr := {getter}(s.ptr)"));
                    w.block("if cStr == nil {", "}", |w| {
                        w.line("return nil");
                    });
                    w.line("v := C.GoString(cStr)");
                    w.line("return &v");
                }
                TypeRef::TypedHandle(n) => {
                    let inner_go = local_type_name(n).to_upper_camel_case();
                    w.line(format!("cPtr := {getter}(s.ptr)"));
                    w.block("if cPtr == nil {", "}", |w| {
                        w.line("return nil");
                    });
                    w.line(format!("return &{inner_go}{{ptr: cPtr}}"));
                }
                TypeRef::Struct(n) => {
                    let inner_go = local_type_name(n).to_upper_camel_case();
                    w.line(format!("cPtr := {getter}(s.ptr)"));
                    w.block("if cPtr == nil {", "}", |w| {
                        w.line("return nil");
                    });
                    w.line(format!("return &{inner_go}{{ptr: cPtr}}"));
                }
                TypeRef::Bool => {
                    w.line(format!("cVal := {getter}(s.ptr)"));
                    w.block("if cVal == nil {", "}", |w| {
                        w.line("return nil");
                    });
                    w.line("v := cToBool(*cVal)");
                    w.line("return &v");
                }
                _ => {
                    let inner_go = go_type(inner);
                    w.line(format!("cVal := {getter}(s.ptr)"));
                    w.block("if cVal == nil {", "}", |w| {
                        w.line("return nil");
                    });
                    w.line(format!("v := {inner_go}(*cVal)"));
                    w.line("return &v");
                }
            },
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line("var cOutLen C.size_t");
                w.line(format!("result := {getter}(s.ptr, &cOutLen)"));
                w.block("if result == nil {", "}", |w| {
                    w.line("return nil");
                });
                w.line("return C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))");
            }
            TypeRef::List(inner) => {
                w.line("var cOutLen C.size_t");
                w.line(format!("result := {getter}(s.ptr, &cOutLen)"));
                w.line("count := int(cOutLen)");
                let mut body = String::new();
                decode_list(
                    &mut body, "goResult", inner, "result", "count", prefix, module,
                );
                w.raw(body);
                w.line("return goResult");
            }
            TypeRef::Map(k, v) => {
                let kt = go_cmap_ptr_type(k, prefix, module);
                let vt = go_cmap_ptr_type(v, prefix, module);
                w.line(format!("var cMapKeys {kt}"));
                w.line(format!("var cMapVals {vt}"));
                w.line("var cOutLen C.size_t");
                w.line(format!("{getter}(s.ptr, &cMapKeys, &cMapVals, &cOutLen)"));
                w.line("count := int(cOutLen)");
                let mut body = String::new();
                decode_map(
                    &mut body, "goResult", k, v, "cMapKeys", "cMapVals", "count", prefix, module,
                );
                w.raw(body);
                w.line("return goResult");
            }
            _ => {
                w.line(format!("return {ret}({getter}(s.ptr))"));
            }
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Functions ──

fn render_function(out: &mut String, prefix: &str, module: &str, f: &FnBinding) {
    let c_sym = &f.c_base;
    let go_name = format!("{}_{}", module, f.name).to_upper_camel_case();

    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();

    let ret_sig = match &f.ret {
        Some(ret) => format!("({}, error)", go_type(ret)),
        None => "error".into(),
    };

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", &go_name);
    w.raw(doc);
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();

    for p in &f.params {
        emit_param(
            &mut pre,
            &mut c_args,
            &p.name.to_lower_camel_case(),
            &p.ty,
            prefix,
            module,
        );
    }

    // An iterator-returning function launches an opaque iterator (no out_len),
    // then this wrapper drains it via the `next`/`destroy` symbols into a slice.
    if let CallShape::Iterator(ib) = &f.shape {
        let mut body = String::new();
        emit_iterator_body(&mut body, &mut pre, &mut c_args, ib, prefix, module);
        w.line(format!(
            "func {go_name}({}) {ret_sig} {{",
            go_params.join(", ")
        ));
        w.raw(body);
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
        return;
    }

    if let Some(ref ret) = f.ret {
        emit_return_out_params(&mut pre, &mut c_args, ret, prefix, module);
    }

    pre.push_str("\tvar cErr C.weaveffi_error\n");
    c_args.push("&cErr".into());

    let args = c_args.join(", ");
    let c_returns_void = matches!(&f.ret, Some(TypeRef::Map(_, _)));

    w.block(
        format!("func {go_name}({}) {ret_sig} {{", go_params.join(", ")),
        "}",
        |w| {
            w.raw(pre.as_str());

            if f.ret.is_some() && !c_returns_void {
                w.line(format!("result := C.{c_sym}({args})"));
            } else {
                w.line(format!("C.{c_sym}({args})"));
            }

            w.block("if cErr.code != 0 {", "}", |w| {
                w.line("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))");
                w.line("C.weaveffi_error_clear(&cErr)");
                if let Some(ref ret) = f.ret {
                    w.line(format!("return {}, goErr", go_zero(ret)));
                } else {
                    w.line("return goErr");
                }
            });

            if let Some(ref ret) = f.ret {
                let mut tail = String::new();
                emit_return(&mut tail, ret, prefix, module);
                w.raw(tail);
            } else {
                w.line("return nil");
            }
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Go type of the `out_item` local whose address is passed to an iterator's
/// `next` (the C slot is `T*`, so the local is one indirection less).
fn iter_out_item_type(inner: &TypeRef, prefix: &str, module: &str) -> String {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "*C.char".into(),
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) => {
            format!("*C.{}", c_abi_struct_name(n, module, prefix))
        }
        _ => c_scalar_type(inner, prefix, module).unwrap_or_else(|| "C.int64_t".into()),
    }
}

/// Append one freshly-pulled iterator element (`item`) to the result slice,
/// converting to the Go type and releasing any callee-allocated string.
fn emit_iter_elem_append(out: &mut String, dst: &str, inner: &TypeRef, item: &str) {
    let mut w = CodeWriter::tabs().with_depth(2);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{dst} = append({dst}, C.GoString({item}))"));
            w.line(format!("C.weaveffi_free_string({item})"));
        }
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) => {
            let gs = local_type_name(n).to_upper_camel_case();
            w.line(format!("{dst} = append({dst}, &{gs}{{ptr: {item}}})"));
        }
        TypeRef::Bool => {
            w.line(format!("{dst} = append({dst}, cToBool({item}))"));
        }
        _ => {
            let conv = go_scalar_conv(item, inner);
            w.line(format!("{dst} = append({dst}, {conv})"));
        }
    }
    out.push_str(&w.finish());
}

/// Emit the launch + drain + destroy body of an iterator-returning function.
/// `pre` already holds the input-parameter staging and `c_args` the launch
/// arguments (before `out_err`).
fn emit_iterator_body(
    out: &mut String,
    pre: &mut String,
    c_args: &mut Vec<String>,
    ib: &weaveffi_core::model::IteratorBinding,
    prefix: &str,
    module: &str,
) {
    pre.push_str("\tvar cErr C.weaveffi_error\n");
    c_args.push("&cErr".into());

    let elem = &ib.elem;
    let item_ty = iter_out_item_type(elem, prefix, module);

    let mut w = CodeWriter::tabs().with_depth(1);
    w.raw(pre.as_str());
    w.line(format!(
        "it := C.{}({})",
        ib.launch.symbol,
        c_args.join(", ")
    ));
    w.block("if cErr.code != 0 {", "}", |w| {
        w.line("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))");
        w.line("C.weaveffi_error_clear(&cErr)");
        w.line("return nil, goErr");
    });
    w.line(format!("defer C.{}(it)", ib.destroy_symbol));
    w.line(format!("goResult := []{}{{}}", go_type(elem)));
    w.block("for {", "}", |w| {
        w.line(format!("var outItem {item_ty}"));
        w.line("var iterErr C.weaveffi_error");
        w.block(
            format!("if C.{}(it, &outItem, &iterErr) == 0 {{", ib.next.symbol),
            "}",
            |w| {
                w.line("break");
            },
        );
        w.block("if iterErr.code != 0 {", "}", |w| {
            w.line("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(iterErr.message), int(iterErr.code))");
            w.line("C.weaveffi_error_clear(&iterErr)");
            w.line("return nil, goErr");
        });
        let mut app = String::new();
        emit_iter_elem_append(&mut app, "goResult", elem, "outItem");
        w.raw(app);
    });
    w.line("return goResult, nil");
    out.push_str(&w.finish());
}

// ── Parameter conversion ──

fn emit_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    ty: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64 => {
            args.push(c_scalar_conv(name, ty, prefix, module));
        }
        TypeRef::Bool => args.push(format!("boolToC({name})")),
        TypeRef::Handle => args.push(format!("C.weaveffi_handle_t({name})")),
        TypeRef::Enum(n) => args.push(format!(
            "C.{}({name})",
            c_abi_struct_name(n, module, prefix)
        )),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => args.push(format!("{name}.ptr")),

        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let cv = format!("c{}", name.to_upper_camel_case());
            w.line(format!("{cv} := C.CString({name})"));
            w.line(format!("defer C.free(unsafe.Pointer({cv}))"));
            args.push(cv);
        }

        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let pv = format!("c{}Ptr", name.to_upper_camel_case());
            let lv = format!("c{}Len", name.to_upper_camel_case());
            w.line(format!("var {pv} *C.uint8_t"));
            w.line(format!("{lv} := C.size_t(len({name}))"));
            w.block(format!("if len({name}) > 0 {{"), "}", |w| {
                w.line(format!("{pv} = (*C.uint8_t)(unsafe.Pointer(&{name}[0]))"));
            });
            args.push(pv);
            args.push(lv);
        }

        TypeRef::Optional(inner) => {
            return emit_optional_param(pre, args, name, inner, prefix, module)
        }
        TypeRef::List(inner) => return emit_list_param(pre, args, name, inner, prefix, module),
        TypeRef::Map(k, v) => return emit_map_param(pre, args, name, k, v, prefix, module),

        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
    pre.push_str(&w.finish());
}

fn emit_optional_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    inner: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let cv = format!("c{}", name.to_upper_camel_case());

    let mut w = CodeWriter::tabs().with_depth(1);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("var {cv} *C.char"));
            w.block(format!("if {name} != nil {{"), "}", |w| {
                w.line(format!("{cv} = C.CString(*{name})"));
                w.line(format!("defer C.free(unsafe.Pointer({cv}))"));
            });
            args.push(cv);
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            let ct = c_opaque_type(inner, prefix, module);
            w.line(format!("var {cv} *C.{ct}"));
            w.block(format!("if {name} != nil {{"), "}", |w| {
                w.line(format!("{cv} = {name}.ptr"));
            });
            args.push(cv);
        }
        _ => {
            if let Some(ct) = c_scalar_type(inner, prefix, module) {
                w.line(format!("var {cv} *{ct}"));
                let conv = c_scalar_conv(&format!("*{name}"), inner, prefix, module);
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!("tmp := {conv}"));
                    w.line(format!("{cv} = &tmp"));
                });
                args.push(cv);
            } else {
                args.push(name.to_string());
            }
        }
    }
    pre.push_str(&w.finish());
}

fn emit_list_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    inner: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let cn = name.to_upper_camel_case();
    let pv = format!("c{cn}Ptr");
    let lv = format!("c{cn}Len");

    let mut w = CodeWriter::tabs().with_depth(1);
    w.line(format!("{lv} := C.size_t(len({name}))"));

    if let Some(ct) = c_scalar_type(inner, prefix, module) {
        if matches!(inner, TypeRef::Bool) {
            let arr = format!("c{cn}Arr");
            w.line(format!("{arr} := make([]C._Bool, len({name}))"));
            w.block(format!("for i, b := range {name} {{"), "}", |w| {
                w.line(format!("{arr}[i] = boolToC(b)"));
            });
            w.line(format!("var {pv} *C._Bool"));
            w.block(format!("if len({arr}) > 0 {{"), "}", |w| {
                w.line(format!("{pv} = &{arr}[0]"));
            });
        } else {
            w.line(format!("var {pv} *{ct}"));
            w.block(format!("if len({name}) > 0 {{"), "}", |w| {
                w.line(format!("{pv} = (*{ct})(unsafe.Pointer(&{name}[0]))"));
            });
        }
    } else if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let arr = format!("c{cn}Arr");
        w.line(format!("{arr} := make([]*C.char, len({name}))"));
        w.block(format!("for i, s := range {name} {{"), "}", |w| {
            w.line(format!("{arr}[i] = C.CString(s)"));
        });
        w.block("defer func() {", "}()", |w| {
            w.block(format!("for _, p := range {arr} {{"), "}", |w| {
                w.line("C.free(unsafe.Pointer(p))");
            });
        });
        w.line(format!("var {pv} **C.char"));
        w.block(format!("if len({arr}) > 0 {{"), "}", |w| {
            w.line(format!("{pv} = (**C.char)(unsafe.Pointer(&{arr}[0]))"));
        });
    } else if let TypeRef::Struct(n) | TypeRef::TypedHandle(n) = inner {
        let ct = format!("C.{}", c_abi_struct_name(n, module, prefix));
        let arr = format!("c{cn}Arr");
        w.line(format!("{arr} := make([]*{ct}, len({name}))"));
        w.block(format!("for i, item := range {name} {{"), "}", |w| {
            w.line(format!("{arr}[i] = item.ptr"));
        });
        w.line(format!("var {pv} **{ct}"));
        w.block(format!("if len({arr}) > 0 {{"), "}", |w| {
            w.line(format!("{pv} = (**{ct})(unsafe.Pointer(&{arr}[0]))"));
        });
    } else {
        w.line(format!("var {pv} unsafe.Pointer"));
    }

    pre.push_str(&w.finish());
    args.push(pv);
    args.push(lv);
}

fn emit_map_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    k: &TypeRef,
    v: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let cn = name.to_upper_camel_case();
    let lv = format!("c{cn}Len");
    let go_k = go_type(k);
    let go_v = go_type(v);

    let mut w = CodeWriter::tabs().with_depth(1);
    w.line(format!("{lv} := C.size_t(len({name}))"));
    w.line(format!("keys{cn} := make([]{go_k}, 0, len({name}))"));
    w.line(format!("vals{cn} := make([]{go_v}, 0, len({name}))"));
    w.block(format!("for mk, mv := range {name} {{"), "}", |w| {
        w.line(format!("keys{cn} = append(keys{cn}, mk)"));
        w.line(format!("vals{cn} = append(vals{cn}, mv)"));
    });
    pre.push_str(&w.finish());

    let kp = format!("c{cn}KeysPtr");
    emit_map_array(pre, &kp, &format!("keys{cn}"), k, prefix, module);
    args.push(kp);

    let vp = format!("c{cn}ValsPtr");
    emit_map_array(pre, &vp, &format!("vals{cn}"), v, prefix, module);
    args.push(vp);

    args.push(lv);
}

fn emit_map_array(
    pre: &mut String,
    ptr_var: &str,
    slice_name: &str,
    ty: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    if let Some(ct) = c_scalar_type(ty, prefix, module) {
        w.line(format!("var {ptr_var} *{ct}"));
        w.block(format!("if len({slice_name}) > 0 {{"), "}", |w| {
            w.line(format!(
                "{ptr_var} = (*{ct})(unsafe.Pointer(&{slice_name}[0]))"
            ));
        });
    } else if matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let arr = format!("{ptr_var}Arr");
        w.line(format!("{arr} := make([]*C.char, len({slice_name}))"));
        w.block(format!("for i, s := range {slice_name} {{"), "}", |w| {
            w.line(format!("{arr}[i] = C.CString(s)"));
        });
        w.block("defer func() {", "}()", |w| {
            w.block(format!("for _, p := range {arr} {{"), "}", |w| {
                w.line("C.free(unsafe.Pointer(p))");
            });
        });
        w.line(format!("var {ptr_var} **C.char"));
        w.block(format!("if len({arr}) > 0 {{"), "}", |w| {
            w.line(format!("{ptr_var} = (**C.char)(unsafe.Pointer(&{arr}[0]))"));
        });
    } else {
        w.line(format!("var {ptr_var} unsafe.Pointer"));
    }
    pre.push_str(&w.finish());
}

// ── Return out-params ──

fn emit_return_out_params(
    pre: &mut String,
    args: &mut Vec<String>,
    ty: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    match ty {
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("var cOutLen C.size_t");
            args.push("&cOutLen".into());
        }
        TypeRef::Map(k, v) => {
            let kt = go_cmap_ptr_type(k, prefix, module);
            let vt = go_cmap_ptr_type(v, prefix, module);
            w.line(format!("var cMapKeys {kt}"));
            w.line(format!("var cMapVals {vt}"));
            w.line("var cOutLen C.size_t");
            args.push("&cMapKeys".into());
            args.push("&cMapVals".into());
            args.push("&cOutLen".into());
        }
        TypeRef::Optional(inner) => {
            return emit_return_out_params(pre, args, inner, prefix, module)
        }
        _ => {}
    }
    pre.push_str(&w.finish());
}

// ── Return conversion ──

fn emit_return(out: &mut String, ty: &TypeRef, prefix: &str, module: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::F32
        | TypeRef::F64 => {
            let conv = go_scalar_conv("result", ty);
            w.line(format!("return {conv}, nil"));
        }
        TypeRef::Bool => {
            w.line("return cToBool(result), nil");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("goResult := C.GoString(result)");
            w.line("C.weaveffi_free_string(result)");
            w.line("return goResult, nil");
        }
        TypeRef::Enum(_) => {
            let conv = go_scalar_conv("result", ty);
            w.line(format!("return {conv}, nil"));
        }
        TypeRef::TypedHandle(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("return &{g}{{ptr: result}}, nil"));
        }
        TypeRef::Struct(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("return &{g}{{ptr: result}}, nil"));
        }
        TypeRef::Optional(inner) => return emit_optional_return(out, inner, module),
        TypeRef::List(inner) => return emit_list_return(out, inner, prefix, module),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.block("if result == nil {", "}", |w| {
                w.line("return nil, nil");
            });
            w.line("goResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))");
            w.line("C.weaveffi_free_bytes(result, cOutLen)");
            w.line("return goResult, nil");
        }
        TypeRef::Map(k, v) => return emit_map_return(out, k, v, prefix, module),
        TypeRef::Iterator(inner) => return emit_list_return(out, inner, prefix, module),
    }
    out.push_str(&w.finish());
}

fn emit_optional_return(out: &mut String, inner: &TypeRef, _module: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    w.block("if result == nil {", "}", |w| {
        w.line("return nil, nil");
    });
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("v := C.GoString(result)");
            w.line("C.weaveffi_free_string(result)");
            w.line("return &v, nil");
        }
        TypeRef::TypedHandle(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("return &{g}{{ptr: result}}, nil"));
        }
        TypeRef::Struct(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("return &{g}{{ptr: result}}, nil"));
        }
        TypeRef::Bool => {
            w.line("v := cToBool(*result)");
            w.line("return &v, nil");
        }
        _ => {
            let gt = go_type(inner);
            w.line(format!("v := {gt}(*result)"));
            w.line("return &v, nil");
        }
    }
    out.push_str(&w.finish());
}

fn emit_list_return(out: &mut String, inner: &TypeRef, prefix: &str, module: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    w.line("count := int(cOutLen)");
    let mut body = String::new();
    decode_list(
        &mut body, "goResult", inner, "result", "count", prefix, module,
    );
    w.raw(body);
    w.line("return goResult, nil");
    out.push_str(&w.finish());
}

fn emit_map_return(out: &mut String, k: &TypeRef, v: &TypeRef, prefix: &str, module: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    w.line("count := int(cOutLen)");
    let mut body = String::new();
    decode_map(
        &mut body, "goResult", k, v, "cMapKeys", "cMapVals", "count", prefix, module,
    );
    w.raw(body);
    w.line("return goResult, nil");
    out.push_str(&w.finish());
}

/// Go type of the local variable whose address is passed for an
/// `out_keys`/`out_values` map out-parameter. The C parameter is `K**`/`V**`
/// (e.g. `const char***` for string keys), so the variable is one indirection
/// less because we pass its address.
fn go_cmap_ptr_type(ty: &TypeRef, prefix: &str, module: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "**C.char".into(),
        _ => format!(
            "*{}",
            c_scalar_type(ty, prefix, module).unwrap_or_else(|| "C.int64_t".into())
        ),
    }
}

/// Read one map key/value from a typed cgo slice index expression.
fn map_elem_read(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("C.GoString({expr})"),
        _ => go_scalar_conv(expr, ty),
    }
}

/// Emit Go that materializes a C array (`src`, `count` elements of `inner`) into
/// a fresh slice bound to `dst`. Shared by struct getters and function returns.
fn decode_list(
    out: &mut String,
    dst: &str,
    inner: &TypeRef,
    src: &str,
    count: &str,
    prefix: &str,
    module: &str,
) {
    let gi = go_type(inner);
    let mut w = CodeWriter::tabs().with_depth(1);
    w.line(format!("{dst} := make([]{gi}, {count})"));
    w.block(format!("if {count} > 0 && {src} != nil {{"), "}", |w| {
        if let Some(ct) = c_scalar_type(inner, prefix, module) {
            w.block(
                format!(
                    "for i, v := range unsafe.Slice((*{ct})(unsafe.Pointer({src})), {count}) {{"
                ),
                "}",
                |w| {
                    let conv = go_scalar_conv("v", inner);
                    w.line(format!("{dst}[i] = {conv}"));
                },
            );
        } else if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
            w.block(
                format!(
                    "for i, v := range unsafe.Slice((**C.char)(unsafe.Pointer({src})), {count}) {{"
                ),
                "}",
                |w| {
                    w.line(format!("{dst}[i] = C.GoString(v)"));
                },
            );
        } else if let TypeRef::TypedHandle(n) | TypeRef::Struct(n) = inner {
            let ct = format!("C.{}", c_abi_struct_name(n, module, prefix));
            let gs = local_type_name(n).to_upper_camel_case();
            w.block(
                format!(
                    "for i, v := range unsafe.Slice((**{ct})(unsafe.Pointer({src})), {count}) {{"
                ),
                "}",
                |w| {
                    w.line(format!("{dst}[i] = &{gs}{{ptr: v}}"));
                },
            );
        }
    });
    out.push_str(&w.finish());
}

/// Emit Go that materializes parallel C key/value arrays (`keys`/`vals`, already
/// typed per [`go_cmap_ptr_type`]) into a fresh map bound to `dst`.
#[allow(clippy::too_many_arguments)]
fn decode_map(
    out: &mut String,
    dst: &str,
    k: &TypeRef,
    v: &TypeRef,
    keys: &str,
    vals: &str,
    count: &str,
    _prefix: &str,
    _module: &str,
) {
    let gk = go_type(k);
    let gv = go_type(v);
    let mut w = CodeWriter::tabs().with_depth(1);
    w.line(format!("{dst} := make(map[{gk}]{gv}, {count})"));
    w.block(
        format!("if {count} > 0 && {keys} != nil && {vals} != nil {{"),
        "}",
        |w| {
            w.line(format!("keySlice := unsafe.Slice({keys}, {count})"));
            w.line(format!("valSlice := unsafe.Slice({vals}, {count})"));
            w.block(format!("for i := 0; i < {count}; i++ {{"), "}", |w| {
                let kr = map_elem_read("keySlice[i]", k);
                let vr = map_elem_read("valSlice[i]", v);
                w.line(format!("{dst}[{kr}] = {vr}"));
            });
        },
    );
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;

    #[test]
    fn package_rewrites_cgo_and_bundles_libs() {
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = calculator_api();
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        // Mirror the CLI: the config basename drives the `-l<name>` link name,
        // which must match the bundled library's base name.
        let cfg = GoConfig {
            input_basename: Some("calculator.yml".into()),
            ..GoConfig::default()
        };
        let files = LanguageBackend::package(
            &GoGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &cfg,
        )
        .expect("go supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        let go = files
            .iter()
            .find(|f| f.path.as_str().ends_with("go/weaveffi.go"))
            .expect("go source present");
        let FileContent::Text(src) = &go.content else {
            panic!("go source is text");
        };
        assert!(
            src.contains("#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/darwin-arm64"),
            "cgo preamble not rewritten: {src}"
        );
        assert!(src.contains("#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows-x64"));
        assert!(src.contains("#cgo LDFLAGS: -lcalculator"));
    }
    use weaveffi_ir::ir::{
        Api, CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
        StructField, TypeRef,
    };

    fn calculator_api() -> Api {
        Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "calculator".into(),
                functions: vec![
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
                    Function {
                        name: "echo".into(),
                        params: vec![Param {
                            name: "msg".into(),
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
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        }
    }

    #[test]
    fn name_returns_go() {
        assert_eq!(Generator::name(&GoGenerator), "go");
    }

    #[test]
    fn output_files_correct() {
        let api = calculator_api();
        let out = Utf8Path::new("out");
        let files = GoGenerator.output_files(&api, out, &GoConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out}/go/README.md"),
                format!("{out}/go/go.mod"),
                format!("{out}/go/weaveffi.go"),
            ]
        );
    }

    #[test]
    fn package_and_cgo_preamble() {
        let go = render_go(&calculator_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("package weaveffi\n"), "missing package");
        assert!(
            go.contains("#cgo LDFLAGS: -lweaveffi"),
            "missing LDFLAGS: {go}"
        );
        assert!(
            go.contains("#include \"weaveffi.h\""),
            "missing weaveffi.h include: {go}"
        );
        assert!(go.contains("import \"C\""), "missing import C: {go}");
    }

    /// Async functions get a blocking wrapper: a registry-id context, an
    /// exported completion trampoline, and a buffered channel the wrapper
    /// waits on. The channel is buffered so the producer thread never blocks
    /// on the send even if the waiter has already given up.
    #[test]
    fn go_async_generates_blocking_wrapper() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![
                    Function {
                        name: "read".into(),
                        params: vec![],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "write".into(),
                        params: vec![],
                        returns: None,
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("//export goWv_weaveffi_io_read_callback"),
            "completion trampoline must be exported: {go}"
        );
        assert!(
            go.contains("extern void goWv_weaveffi_io_read_callback(void* context, weaveffi_error* err, char* result);"),
            "preamble must declare the trampoline const-free: {go}"
        );
        assert!(
            go.contains("C.weaveffi_io_read_async("),
            "async launcher must be invoked: {go}"
        );
        assert!(
            go.contains("func IoRead() (string, error) {"),
            "blocking wrapper must be emitted: {go}"
        );
        assert!(
            go.contains("ch := make(chan wvOutcomeIoRead, 1)"),
            "wrapper must wait on a buffered outcome channel: {go}"
        );
        assert!(
            go.contains("C.weaveffi_free_string(result)"),
            "owned string results must be freed: {go}"
        );
        assert!(
            go.contains("weaveffi_io_write"),
            "sync function should still be emitted: {go}"
        );
        assert!(go.contains("\t\"sync\"\n"), "sync import needed: {go}");
    }

    #[test]
    fn imports_fmt_and_unsafe() {
        let go = render_go(&calculator_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("\"fmt\""), "missing fmt import: {go}");
        assert!(go.contains("\"unsafe\""), "missing unsafe import: {go}");
    }

    #[test]
    fn simple_i32_function() {
        let go = render_go(&calculator_api(), "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func CalculatorAdd(a int32, b int32) (int32, error)"),
            "missing function sig: {go}"
        );
        assert!(
            go.contains("C.weaveffi_calculator_add("),
            "missing C call: {go}"
        );
        assert!(go.contains("C.int32_t(a)"), "missing param cast: {go}");
        assert!(
            go.contains("return int32(result), nil"),
            "missing return: {go}"
        );
    }

    #[test]
    fn string_function() {
        let go = render_go(&calculator_api(), "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func CalculatorEcho(msg string) (string, error)"),
            "missing echo sig: {go}"
        );
        assert!(go.contains("C.CString(msg)"), "missing CString: {go}");
        assert!(
            go.contains("defer C.free(unsafe.Pointer("),
            "missing defer free: {go}"
        );
        assert!(go.contains("C.GoString(result)"), "missing GoString: {go}");
        assert!(
            go.contains("C.weaveffi_free_string(result)"),
            "missing free_string: {go}"
        );
    }

    #[test]
    fn error_handling() {
        let go = render_go(&calculator_api(), "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("var cErr C.weaveffi_error"),
            "missing error var: {go}"
        );
        assert!(
            go.contains("if cErr.code != 0"),
            "missing error check: {go}"
        );
        assert!(
            go.contains("C.weaveffi_error_clear(&cErr)"),
            "missing error clear: {go}"
        );
        assert!(go.contains("fmt.Errorf("), "missing Errorf: {go}");
    }

    #[test]
    fn enum_generation() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![],
                structs: vec![],
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("type Color int32"),
            "missing enum typedef: {go}"
        );
        assert!(
            go.contains("ColorRed Color = 0"),
            "missing Red variant: {go}"
        );
        assert!(
            go.contains("ColorGreen Color = 1"),
            "missing Green variant: {go}"
        );
        assert!(
            go.contains("ColorBlue Color = 2"),
            "missing Blue variant: {go}"
        );
    }

    #[test]
    fn struct_with_getters_and_close() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
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
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(go.contains("type Contact struct {"), "missing struct: {go}");
        assert!(
            go.contains("ptr *C.weaveffi_contacts_Contact"),
            "missing ptr field: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Name() string"),
            "missing Name getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Age() int32"),
            "missing Age getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Close()"),
            "missing Close: {go}"
        );
        assert!(
            go.contains("C.weaveffi_contacts_Contact_destroy(s.ptr)"),
            "missing destroy call: {go}"
        );
        assert!(
            go.contains("s.ptr = nil"),
            "missing nil assignment after destroy: {go}"
        );
    }

    #[test]
    fn struct_builder_type_and_setters() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "geo".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Point".into(),
                    doc: None,
                    builder: true,
                    fields: vec![StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    }],
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
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("type PointBuilder struct {"),
            "builder type: {go}"
        );
        assert!(go.contains("\tx float64\n"), "typed builder field: {go}");
        assert!(
            go.contains("func NewPointBuilder() *PointBuilder"),
            "constructor: {go}"
        );
        assert!(
            go.contains("return &PointBuilder{}"),
            "constructor body: {go}"
        );
        assert!(
            go.contains("func (b *PointBuilder) WithX(value float64) *PointBuilder"),
            "WithX: {go}"
        );
        assert!(go.contains("b.x = value"), "field assign: {go}");
        assert!(
            go.contains("func (b *PointBuilder) Build() (*Point, error)"),
            "Build returns (*Point, error): {go}"
        );
    }

    #[test]
    fn void_function() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "system".into(),
                functions: vec![Function {
                    name: "reset".into(),
                    params: vec![],
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func SystemReset() error"),
            "missing void function sig: {go}"
        );
        assert!(
            go.contains("return nil"),
            "missing nil return for void: {go}"
        );
    }

    #[test]
    fn handle_type() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "create".into(),
                    params: vec![Param {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Handle),
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
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("(int64, error)"),
            "handle return should be int64: {go}"
        );
        assert!(
            go.contains("return int64(result), nil"),
            "missing handle return conversion: {go}"
        );
    }

    #[test]
    fn bool_function_generates_helpers() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "logic".into(),
                functions: vec![Function {
                    name: "negate".into(),
                    params: vec![Param {
                        name: "val".into(),
                        ty: TypeRef::Bool,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Bool),
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
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(go.contains("func boolToC("), "missing boolToC: {go}");
        assert!(go.contains("func cToBool("), "missing cToBool: {go}");
        assert!(
            go.contains("boolToC(val)"),
            "missing boolToC call for param: {go}"
        );
        assert!(
            go.contains("cToBool(result)"),
            "missing cToBool for return: {go}"
        );
    }

    #[test]
    fn enum_param_and_return() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![Function {
                    name: "mix".into(),
                    params: vec![Param {
                        name: "a".into(),
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
                enums: vec![EnumDef {
                    name: "Color".into(),
                    doc: None,
                    variants: vec![EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    }],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func PaintMix(a Color) (Color, error)"),
            "missing enum function sig: {go}"
        );
        assert!(
            go.contains("C.weaveffi_paint_Color(a)"),
            "missing enum param conversion: {go}"
        );
        assert!(
            go.contains("Color(result)"),
            "missing enum return conversion: {go}"
        );
    }

    #[test]
    fn struct_return() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
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
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
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
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("(*Contact, error)"),
            "missing struct return type: {go}"
        );
        assert!(
            go.contains("&Contact{ptr: result}"),
            "missing struct wrap: {go}"
        );
    }

    #[test]
    fn optional_string_param() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "query".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("query *string"),
            "optional string param should be *string: {go}"
        );
        assert!(
            go.contains("if query != nil"),
            "missing nil check for optional: {go}"
        );
        assert!(
            go.contains("C.CString(*query)"),
            "missing CString dereference: {go}"
        );
    }

    #[test]
    fn optional_struct_return() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "find".into(),
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("(*Contact, error)"),
            "optional struct return: {go}"
        );
        assert!(go.contains("if result == nil"), "missing nil check: {go}");
    }

    #[test]
    fn list_return() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "list_ids".into(),
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("([]int32, error)"),
            "missing list return type: {go}"
        );
        assert!(
            go.contains("var cOutLen C.size_t"),
            "missing out_len var: {go}"
        );
        assert!(go.contains("unsafe.Slice("), "missing unsafe.Slice: {go}");
    }

    #[test]
    fn struct_list_return() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
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
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("([]*Contact, error)"),
            "missing struct list return: {go}"
        );
        assert!(
            go.contains("C.weaveffi_contacts_Contact"),
            "missing C struct type in list conversion: {go}"
        );
    }

    #[test]
    fn async_cancellable_passes_null_token() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![Function {
                    name: "run".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: true,
                    cancellable: true,
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
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func TasksRun() (int32, error) {"),
            "async wrapper must be generated: {go}"
        );
        assert!(
            go.contains("C.weaveffi_tasks_run_async(nil, "),
            "cancel token must be passed as NULL: {go}"
        );
    }

    #[test]
    fn listeners_generate_register_unregister() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("//export goWv_weaveffi_events_OnMessage_fn"),
            "callback trampoline must be exported: {go}"
        );
        assert!(
            go.contains(
                "extern void goWv_weaveffi_events_OnMessage_fn(char* message, void* context);"
            ),
            "preamble must declare the trampoline: {go}"
        );
        assert!(
            go.contains(
                "func EventsRegisterMessageListener(callback func(message string)) uint64 {"
            ),
            "register wrapper must be emitted: {go}"
        );
        assert!(
            go.contains("func EventsUnregisterMessageListener(id uint64) {"),
            "unregister wrapper must be emitted: {go}"
        );
        assert!(
            go.contains("C.weaveffi_events_register_message_listener(C.weaveffi_events_OnMessage_fn(unsafe.Pointer(C.goWv_weaveffi_events_OnMessage_fn)), unsafe.Pointer(uintptr(ctxID)))"),
            "register must pass the shared trampoline and registry id: {go}"
        );
        assert!(
            go.contains("wvListenerCtx[id] = ctxID"),
            "subscription must retain the Go callback: {go}"
        );
    }

    #[test]
    fn generates_file_on_disk() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go_file = tmp.join("go/weaveffi.go");
        assert!(go_file.exists(), "go/weaveffi.go should exist");
        let contents = std::fs::read_to_string(&go_file).unwrap();
        assert!(
            contents.contains("package weaveffi"),
            "file should contain package declaration"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_generates_go_mod() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_mod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go_mod_path = tmp.join("go/go.mod");
        assert!(go_mod_path.exists(), "go/go.mod should exist");
        let go_mod = std::fs::read_to_string(&go_mod_path).unwrap();
        assert!(
            go_mod.contains("module weaveffi"),
            "missing module directive: {go_mod}"
        );
        assert!(go_mod.contains("go 1.21"), "missing go version: {go_mod}");

        let readme_path = tmp.join("go/README.md");
        assert!(readme_path.exists(), "go/README.md should exist");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        assert!(
            readme.contains("CGo"),
            "README should mention CGo: {readme}"
        );
        assert!(
            readme.contains("go build"),
            "README should mention go build: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn optional_i32_param() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
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
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("id *int32"),
            "optional i32 param should be *int32: {go}"
        );
        assert!(
            go.contains("var cId *C.int32_t"),
            "missing C var for optional: {go}"
        );
    }

    #[test]
    fn struct_optional_string_field() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    }],
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
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func (s *Contact) Email() *string"),
            "optional string getter should return *string: {go}"
        );
        assert!(
            go.contains("if cStr == nil"),
            "should check nil for optional string: {go}"
        );
    }

    #[test]
    fn no_bool_helpers_when_unneeded() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
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
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            !go.contains("boolToC"),
            "should not include bool helpers: {go}"
        );
    }

    #[test]
    fn struct_enum_field_getter() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![EnumDef {
                    name: "ContactType".into(),
                    doc: None,
                    variants: vec![EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    }],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let go = render_go(&api, "weaveffi", "weaveffi.yml");
        assert!(
            go.contains("func (s *Contact) ContactType() ContactType"),
            "missing enum field getter: {go}"
        );
    }

    #[test]
    fn generate_go_basic() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(go.contains("package weaveffi"), "missing package: {go}");
        assert!(
            go.contains("func CalculatorAdd(a int32, b int32) (int32, error)"),
            "missing add function: {go}"
        );
        assert!(
            go.contains("func CalculatorEcho(msg string) (string, error)"),
            "missing echo function: {go}"
        );

        let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
        assert!(
            go_mod.contains("module weaveffi"),
            "go.mod should have default module path: {go_mod}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_with_structs() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
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
                    builder: false,
                    fields: vec![
                        StructField {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "last_name".into(),
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
        let tmp = std::env::temp_dir().join("weaveffi_test_go_structs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(go.contains("type Contact struct {"), "missing struct: {go}");
        assert!(
            go.contains("ptr *C.weaveffi_contacts_Contact"),
            "missing ptr field: {go}"
        );
        assert!(
            go.contains("func (s *Contact) FirstName() string"),
            "missing FirstName getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) LastName() string"),
            "missing LastName getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Age() int32"),
            "missing Age getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Close()"),
            "missing Close: {go}"
        );
        assert!(
            go.contains("(*Contact, error)"),
            "missing struct return type: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_with_enums() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "classify".into(),
                    params: vec![Param {
                        name: "ct".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Enum("ContactType".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "ContactType".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Personal".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Work".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Other".into(),
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
            }],
            generators: None,
            package: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(
            go.contains("type ContactType int32"),
            "missing enum type: {go}"
        );
        assert!(
            go.contains("ContactTypePersonal ContactType = 0"),
            "missing Personal variant: {go}"
        );
        assert!(
            go.contains("ContactTypeWork ContactType = 1"),
            "missing Work variant: {go}"
        );
        assert!(
            go.contains("ContactTypeOther ContactType = 2"),
            "missing Other variant: {go}"
        );
        assert!(
            go.contains("func ContactsClassify(ct ContactType) (ContactType, error)"),
            "missing classify function: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_error_handling() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![
                    Function {
                        name: "save".into(),
                        params: vec![Param {
                            name: "data".into(),
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
                    },
                    Function {
                        name: "clear".into(),
                        params: vec![],
                        returns: None,
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
            }],
            generators: None,
            package: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_errors");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(
            go.contains("func StoreSave(data string) (int32, error)"),
            "missing save sig: {go}"
        );
        assert!(
            go.contains("func StoreClear() error"),
            "missing void clear sig: {go}"
        );
        assert!(
            go.contains("var cErr C.weaveffi_error"),
            "missing error var: {go}"
        );
        assert!(
            go.contains("if cErr.code != 0"),
            "missing error check: {go}"
        );
        assert!(
            go.contains("C.weaveffi_error_clear(&cErr)"),
            "missing error clear: {go}"
        );
        assert!(
            go.contains("return 0, goErr"),
            "missing zero-value error return for i32: {go}"
        );
        assert!(
            go.contains("return goErr"),
            "missing void error return: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_full_contacts() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![
                    Function {
                        name: "create_contact".into(),
                        params: vec![
                            Param {
                                name: "first_name".into(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                                doc: None,
                            },
                            Param {
                                name: "last_name".into(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                                doc: None,
                            },
                            Param {
                                name: "email".into(),
                                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                                mutable: false,
                                doc: None,
                            },
                            Param {
                                name: "contact_type".into(),
                                ty: TypeRef::Enum("ContactType".into()),
                                mutable: false,
                                doc: None,
                            },
                        ],
                        returns: Some(TypeRef::Handle),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "get_contact".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::Handle,
                            mutable: false,
                            doc: None,
                        }],
                        returns: Some(TypeRef::Struct("Contact".into())),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "list_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "delete_contact".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::Handle,
                            mutable: false,
                            doc: None,
                        }],
                        returns: Some(TypeRef::Bool),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "count_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![
                        StructField {
                            name: "id".into(),
                            ty: TypeRef::I64,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            doc: None,
                            default: None,
                        },
                    ],
                }],
                enums: vec![EnumDef {
                    name: "ContactType".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Personal".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Work".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Other".into(),
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
            }],
            generators: None,
            package: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_full_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate(&api, out_dir, &GoConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();

        assert!(
            go.contains("type ContactType int32"),
            "missing ContactType enum: {go}"
        );
        assert!(
            go.contains("ContactTypePersonal ContactType = 0"),
            "missing Personal: {go}"
        );
        assert!(
            go.contains("type Contact struct {"),
            "missing Contact struct: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Id() int64"),
            "missing Id getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) FirstName() string"),
            "missing FirstName getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Email() *string"),
            "missing optional Email getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) ContactType() ContactType"),
            "missing ContactType getter: {go}"
        );
        assert!(
            go.contains("func ContactsCreateContact("),
            "missing create_contact: {go}"
        );
        assert!(
            go.contains("(int64, error)"),
            "create_contact should return handle: {go}"
        );
        assert!(
            go.contains("func ContactsGetContact(id int64) (*Contact, error)"),
            "missing get_contact: {go}"
        );
        assert!(
            go.contains("func ContactsListContacts() ([]*Contact, error)"),
            "missing list_contacts: {go}"
        );
        assert!(
            go.contains("func ContactsDeleteContact(id int64) (bool, error)"),
            "missing delete_contact: {go}"
        );
        assert!(
            go.contains("func ContactsCountContacts() (int32, error)"),
            "missing count_contacts: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_custom_module_path() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_custom_mod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        let config = GoConfig {
            module_path: Some("github.com/myorg/mylib".into()),
            ..GoConfig::default()
        };
        GoGenerator.generate(&api, out_dir, &config).unwrap();

        let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
        assert!(
            go_mod.contains("module github.com/myorg/mylib"),
            "go.mod should use custom module path: {go_mod}"
        );
        assert!(
            !go_mod.contains("module weaveffi"),
            "go.mod should not use default path: {go_mod}"
        );

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(
            go.contains("package weaveffi"),
            "Go source should still use weaveffi package: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_no_double_free_on_error() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
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
            }],
            generators: None,
            package: None,
        };

        let go = render_go(&api, "weaveffi", "weaveffi.yml");

        let fn_start = go
            .find("func ContactsFindContact(")
            .expect("ContactsFindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        assert!(
            !fn_text.contains("weaveffi_free_string(cName"),
            "borrowed string param must not be freed via weaveffi_free_string: {fn_text}"
        );

        let err_check = fn_text
            .find("if cErr.code != 0")
            .expect("error check in ContactsFindContact");
        let contact_wrap = fn_text
            .find("&Contact{ptr: result}")
            .expect("Contact wrap in ContactsFindContact");
        assert!(
            err_check < contact_wrap,
            "error must be checked before wrapping struct return: {fn_text}"
        );

        assert!(
            go.contains("func (s *Contact) Close()")
                && go.contains("weaveffi_contacts_Contact_destroy(s.ptr)"),
            "struct return type should have Close calling destroy: {go}"
        );
    }

    #[test]
    fn go_null_check_on_optional_return() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
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
            }],
            generators: None,
            package: None,
        };

        let go = render_go(&api, "weaveffi", "weaveffi.yml");

        let fn_start = go
            .find("func ContactsFindContact(")
            .expect("ContactsFindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        let null_check = fn_text
            .find("if result == nil")
            .expect("nil check in ContactsFindContact");
        let contact_wrap = fn_text
            .find("&Contact{ptr: result}")
            .expect("Contact wrap in ContactsFindContact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check nil before wrapping: {fn_text}"
        );
    }

    fn doc_api() -> Api {
        Api {
            version: "0.4.0".into(),
            modules: vec![Module {
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
            }],
            generators: None,
            package: None,
        }
    }

    #[test]
    fn go_emits_doc_on_function() {
        let go = render_go(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("// DocsDoThing: Performs a thing."), "{go}");
    }

    #[test]
    fn go_emits_doc_on_struct() {
        let go = render_go(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("// Item: An item we track."), "{go}");
    }

    #[test]
    fn go_emits_doc_on_enum_variant() {
        let go = render_go(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("// Kind: Kind of item."), "{go}");
        assert!(go.contains("// KindSmall: A small one"), "{go}");
    }

    #[test]
    fn go_emits_doc_on_field() {
        let go = render_go(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("// Id: Stable id"), "{go}");
    }

    #[test]
    fn go_emits_doc_on_param() {
        let go = render_go(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(go.contains("// Parameters:"), "{go}");
        assert!(go.contains("//   - x: the input value"), "{go}");
    }

    #[test]
    fn go_custom_prefix_threads_to_user_symbols() {
        let go = render_go(&calculator_api(), "myffi", "weaveffi.yml");
        // User symbols adopt the configured prefix.
        assert!(
            go.contains("C.myffi_calculator_add("),
            "user symbol should use the custom prefix: {go}"
        );
        assert!(
            !go.contains("weaveffi_calculator_add"),
            "user symbol must not keep the default prefix: {go}"
        );
        // The cgo preamble includes the prefixed C header.
        assert!(
            go.contains("#include \"myffi.h\""),
            "cgo preamble should include the prefixed header: {go}"
        );
        // Runtime ABI helpers exported by weaveffi-abi stay literal.
        assert!(
            go.contains("C.weaveffi_free_string(result)"),
            "runtime helper weaveffi_free_string must stay literal: {go}"
        );
        assert!(
            go.contains("var cErr C.weaveffi_error"),
            "runtime helper weaveffi_error must stay literal: {go}"
        );
    }
}
