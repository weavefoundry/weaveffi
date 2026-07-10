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
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding,
    FieldBinding, FnBinding, InterfaceBinding, ListenerBinding, ModuleBinding, ParamBinding,
    RichVariantBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::platform::Platform;
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`GoGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GoConfig {
    /// Go module path written to `go.mod` (default `"weaveffi"`).
    pub module_path: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// package-level function names, so module `kv`'s `delete` surfaces as
    /// `Delete` rather than `KvDelete`. Set to `false` to restore the
    /// module-prefixed spelling. Interface members are namespaced by their
    /// wrapper type and never carry the module prefix.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the cgo bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for GoConfig {
    fn default() -> Self {
        Self {
            module_path: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
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
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("go");
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                dir.join("weaveffi.go"),
                render_go(
                    api,
                    model,
                    config.prefix(),
                    config.strip_module_prefix,
                    input_basename,
                ),
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
        model: &BindingModel,
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
        let go_src = render_go(
            api,
            model,
            prefix,
            config.strip_module_prefix,
            input_basename,
        )
        .replace(&original, &cgo);

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
        // Structs, interfaces, enums, and typed handles surface as bare local
        // Go types; a cross-module reference (resolved to e.g. `kv.Store`)
        // must name the local `Store` type rather than the qualified `KvStore`.
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
            format!("*{}", local_type_name(n).to_upper_camel_case())
        }
        TypeRef::Enum(n) => local_type_name(n).to_upper_camel_case(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Interface(_) => go_type(inner),
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
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) | TypeRef::Interface(n) => {
            c_abi_struct_name(n, module, prefix)
        }
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

/// What the generated file's preamble must pull in, computed by one pass over
/// the lowered model.
#[derive(Default, Clone, Copy)]
struct Imports {
    /// `fmt` (error formatting); implied by [`err_infra`](Self::err_infra).
    fmt: bool,
    /// `unsafe` (pointer staging for strings/bytes/lists/maps, callback
    /// contexts).
    unsafe_ptr: bool,
    /// The `boolToC`/`cToBool` helpers.
    bool_helpers: bool,
    /// `sync` (the callback registry mutex).
    sync: bool,
    /// The shared error plumbing: the [`ERROR_BRAND`] type plus the
    /// `wvTakeError`/`wvBrandError`/`wvTrap` helpers.
    err_infra: bool,
}

/// Scan the lowered model for everything [`Imports`] tracks. Interface
/// members participate exactly like free functions (via
/// [`ModuleBinding::callables`]).
fn scan_imports(model: &BindingModel) -> Imports {
    let mut any_callable = false;
    let mut has_async = false;
    let mut has_listeners = false;
    let mut has_fallible_ctor = false;
    let mut has_domain = false;
    let mut unsafe_ptr = false;
    let mut bool_helpers = false;

    for m in &model.modules {
        has_listeners |= !m.listeners.is_empty();
        has_domain |= m.declares_error();
        for f in m.callables() {
            any_callable = true;
            has_async |= f.is_async;
            unsafe_ptr |= f.params.iter().any(|p| param_uses_unsafe(&p.ty))
                || f.ret.as_ref().is_some_and(return_uses_unsafe);
            bool_helpers |= f.params.iter().any(|p| type_has_bool(&p.ty))
                || f.ret.as_ref().is_some_and(type_has_bool);
        }
        for s in &m.structs {
            // A builder's `Build` calls the C `create` symbol and returns
            // `(*T, error)`, so it needs the error plumbing like a fallible
            // function does; getters can materialize bytes/list/map, and a
            // builder additionally marshals every field *in* (strings stage
            // through unsafe.Pointer).
            has_fallible_ctor |= s.builder.is_some();
            unsafe_ptr |= s.fields.iter().any(|fld| return_uses_unsafe(&fld.ty))
                || (s.builder.is_some() && s.fields.iter().any(|fld| param_uses_unsafe(&fld.ty)));
            bool_helpers |= s.fields.iter().any(|fld| type_has_bool(&fld.ty));
        }
        for e in &m.enums {
            // A rich (algebraic) enum emits a `New{Enum}{Variant}` constructor
            // per variant that returns `(*T, error)`, and its per-variant field
            // getters/constructor arguments marshal exactly like struct fields.
            if let Some(rich) = &e.rich {
                has_fallible_ctor = true;
                unsafe_ptr |= rich.variants.iter().any(|v| {
                    v.fields
                        .iter()
                        .any(|fld| return_uses_unsafe(&fld.ty) || param_uses_unsafe(&fld.ty))
                });
                bool_helpers |= rich
                    .variants
                    .iter()
                    .any(|v| v.fields.iter().any(|fld| type_has_bool(&fld.ty)));
            }
        }
        for cb in &m.callbacks {
            // A callback trampoline's signature carries the `void* context`
            // slot as unsafe.Pointer, and its parameters decode like returns.
            unsafe_ptr = true;
            bool_helpers |= cb.params.iter().any(|p| type_has_bool(&p.ty));
        }
    }

    // Async launchers and listener registration thread the registry id through
    // the C `void* context`, which always stages through unsafe.Pointer.
    unsafe_ptr |= has_async || has_listeners;
    // Every callable checks its error slot (returning or trapping), so any
    // callable at all pulls in the error plumbing; a declared domain also
    // needs it for the brand-error fallback of its mapping helper.
    let err_infra = any_callable || has_fallible_ctor || has_domain;

    Imports {
        fmt: err_infra,
        unsafe_ptr,
        bool_helpers,
        sync: has_async || has_listeners,
        err_infra,
    }
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

// ── Errors ──

/// How a wrapper body reports a non-zero `weaveffi_error` slot.
///
/// A callable with `throws == true` returns `(T, error)` and maps codes
/// through the declaring module's typed helper (`wvMapKv`), falling back to
/// the generic [`ERROR_BRAND`] struct when no domain is in scope (builders and
/// rich-enum constructors). A callable with `throws == false` has a plain
/// signature and panics via `wvTrap` instead, since a reported error can only
/// be a producer panic or an argument-marshalling failure.
#[derive(Clone, Copy)]
struct ErrCtx<'a> {
    /// `true` when the wrapper returns `(T, error)` and surfaces typed errors.
    throws: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `wvMapKv`); `None`
    /// falls back to the generic `wvBrandError` constructor.
    stem: Option<&'a str>,
}

impl ErrCtx<'_> {
    /// The Go expression converting a taken `(code, message)` pair into an
    /// `error` value.
    fn map_call(&self, args: &str) -> String {
        match self.stem {
            Some(stem) => format!("wvMap{stem}({args})"),
            None => format!("wvBrandError({args})"),
        }
    }

    /// Emit the statement(s) checking the error slot named `slot` at `w`'s
    /// current depth. A throwing wrapper returns `zero` (when the function
    /// has a result) plus the mapped error; a plain wrapper traps.
    fn emit_check(&self, w: &mut CodeWriter, slot: &str, zero: Option<&str>) {
        if self.throws {
            let map = self.map_call(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                match zero {
                    Some(z) => w.line(format!("return {z}, {map}")),
                    None => w.line(format!("return {map}")),
                };
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    }

    /// The Go return-type suffix (including the leading space) of a wrapper
    /// returning `ret`: `(T, error)`/`error` when throwing, `T`/nothing when
    /// plain.
    fn ret_sig(&self, ret: &Option<TypeRef>) -> String {
        match (ret, self.throws) {
            (Some(r), true) => format!(" ({}, error)", go_type(r)),
            (Some(r), false) => format!(" {}", go_type(r)),
            (None, true) => " error".into(),
            (None, false) => String::new(),
        }
    }

    /// The suffix appended to every successful `return` statement: `, nil`
    /// when the wrapper also returns an error, empty otherwise.
    fn ok_tail(&self) -> &'static str {
        if self.throws {
            ", nil"
        } else {
            ""
        }
    }
}

/// The PascalCase helper stem of the domain in effect for `module`, naming
/// the per-domain `wvMap{Stem}` helper (derived from the *declaring* module's
/// path, so inheriting submodules reference the ancestor's helper).
fn domain_stem(module: &ModuleBinding) -> Option<String> {
    module
        .error
        .as_ref()
        .map(|e| e.owner_path.to_upper_camel_case())
}

/// The shared error plumbing: the generic [`ERROR_BRAND`] struct implementing
/// `error` (unknown codes, marshalling failures, builder/rich-enum failures),
/// plus the `wvTakeError` slot reader, the `wvBrandError` constructor, and
/// the `wvTrap` panic helper non-throwing wrappers check their slot with.
fn render_error_infra(out: &mut String) {
    let mut w = CodeWriter::tabs();
    w.line(format!(
        "// {ERROR_BRAND} reports a failure crossing the C boundary that no typed"
    ));
    w.line("// error domain claims: an unknown code, a marshalling failure, or a");
    w.line("// producer panic.");
    w.block(format!("type {ERROR_BRAND} struct {{"), "}", |w| {
        w.line("// Code is the numeric ABI error code.");
        w.line("Code int32");
        w.line("// Message is the human-readable error message.");
        w.line("Message string");
    });
    w.blank();
    w.block(
        format!("func (e *{ERROR_BRAND}) Error() string {{"),
        "}",
        |w| {
            w.line("return fmt.Sprintf(\"weaveffi: %s (code %d)\", e.Message, e.Code)");
        },
    );
    w.blank();

    w.line("// wvTakeError reads and clears a non-zero C error slot, returning its");
    w.line("// code and message.");
    w.block(
        "func wvTakeError(cErr *C.weaveffi_error) (int32, string) {",
        "}",
        |w| {
            w.line("code := int32(cErr.code)");
            w.line("msg := \"\"");
            w.block("if cErr.message != nil {", "}", |w| {
                w.line("msg = C.GoString(cErr.message)");
            });
            w.line("C.weaveffi_error_clear(cErr)");
            w.line("return code, msg");
        },
    );
    w.blank();

    w.block(
        "func wvBrandError(code int32, message string) error {",
        "}",
        |w| {
            w.line(format!(
                "return &{ERROR_BRAND}{{Code: code, Message: message}}"
            ));
        },
    );
    w.blank();

    w.line("// wvTrap panics when the C error slot reports a failure. Non-throwing");
    w.line("// wrappers check their slot with it: a non-zero code there can only be");
    w.line("// a producer panic or a marshalling failure.");
    w.block("func wvTrap(cErr *C.weaveffi_error) {", "}", |w| {
        w.block("if cErr.code != 0 {", "}", |w| {
            w.line("code, msg := wvTakeError(cErr)");
            w.line("panic(fmt.Sprintf(\"weaveffi: %s (code %d)\", msg, code))");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Render one declaring module's typed error surface: a
/// `type {TypeName} struct {{ Code int32; Message string }}` implementing
/// `error` (so `errors.As` selects on the domain), exported `int32` code
/// constants in the plain-enum const style (`{TypeName}{CodePascal}`), and
/// the `wvMap{Stem}` helper converting a non-zero slot's `(code, message)`
/// into the typed error (default message when the slot carried none, generic
/// [`ERROR_BRAND`] fallback for unknown codes).
fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let stem = eb.owner_path.to_upper_camel_case();
    let ty = &eb.type_name;
    let dotted = module.segments.join(".");

    let mut w = CodeWriter::tabs();
    w.line(format!(
        "// {ty} is a typed error reported by the `{dotted}` module."
    ));
    w.block(format!("type {ty} struct {{"), "}", |w| {
        w.line(format!(
            "// Code is the numeric ABI error code (one of the {ty} constants)."
        ));
        w.line("Code int32");
        w.line("// Message is the human-readable error message.");
        w.line("Message string");
    });
    w.blank();
    w.block(format!("func (e *{ty}) Error() string {{"), "}", |w| {
        w.line(format!(
            "return fmt.Sprintf(\"{dotted}: %s (code %d)\", e.Message, e.Code)"
        ));
    });
    w.blank();

    w.line(format!("// {ty} codes."));
    w.block("const (", ")", |w| {
        for c in &eb.codes {
            let cname = format!("{ty}{}", c.name.to_upper_camel_case());
            let doc = c.doc.clone().unwrap_or_else(|| c.message.clone());
            let mut cd = String::new();
            emit_doc(&mut cd, &Some(doc), "\t", Some(&cname));
            w.raw(cd);
            w.line(format!("{cname} int32 = {}", c.value));
        }
    });
    w.blank();

    w.line(format!(
        "// wvMap{stem} converts a non-zero code from the `{dotted}` domain into a"
    ));
    w.line(format!(
        "// *{ty}, falling back to the generic *{ERROR_BRAND} for unknown codes."
    ));
    w.block(
        format!("func wvMap{stem}(code int32, message string) error {{"),
        "}",
        |w| {
            w.line("switch code {");
            for c in &eb.codes {
                let cname = format!("{ty}{}", c.name.to_upper_camel_case());
                w.line(format!("case {cname}:"));
                w.indent();
                w.block("if message == \"\" {", "}", |w| {
                    w.line(format!("message = {}", go_str(&c.message)));
                });
                w.line(format!("return &{ty}{{Code: code, Message: message}}"));
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line("return wvBrandError(code, message)");
            w.dedent();
            w.line("}");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Quote `s` as a Go string literal, escaping backslashes, quotes, and
/// newlines.
fn go_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn render_go(
    api: &Api,
    model: &BindingModel,
    prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let imports = scan_imports(model);
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
        .any(|m| m.callables().any(|f| f.is_async));

    out.push_str(&format!("package {go_pkg}\n\n"));

    out.push_str("/*\n");
    out.push_str(&format!("#cgo LDFLAGS: -l{link_name}\n"));
    out.push_str(&format!("#include \"{prefix}.h\"\n"));
    out.push_str("#include <stdlib.h>\n");
    // Forward declarations for the //export trampolines below. These must
    // mirror the prototypes cgo emits into _cgo_export.h (const-free), and the
    // preamble of a file using //export may only contain declarations.
    for decl in collect_trampoline_externs(model, prefix) {
        out.push_str(&decl);
        out.push('\n');
    }
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    if imports.fmt || imports.unsafe_ptr || imports.sync {
        out.push_str("\nimport (\n");
        if imports.fmt {
            out.push_str("\t\"fmt\"\n");
        }
        if imports.sync {
            out.push_str("\t\"sync\"\n");
        }
        if imports.unsafe_ptr {
            out.push_str("\t\"unsafe\"\n");
        }
        out.push_str(")\n");
    }
    out.push('\n');

    if imports.bool_helpers {
        // cgo models C `_Bool` as a distinct Go type whose underlying kind is
        // bool, so convert with the type itself rather than integer literals.
        out.push_str("func boolToC(b bool) C._Bool {\n");
        out.push_str("\treturn C._Bool(b)\n");
        out.push_str("}\n\n");
        out.push_str("func cToBool(b C._Bool) bool {\n");
        out.push_str("\treturn bool(b)\n");
        out.push_str("}\n\n");
    }

    if imports.err_infra {
        render_error_infra(&mut out);
    }

    if has_async || has_listeners {
        render_callback_registry(&mut out, has_listeners);
    }

    for m in &model.modules {
        let stem = domain_stem(m);
        if let Some(eb) = &m.error {
            // Emit the typed domain once, in its declaring module; inheriting
            // submodules reference the ancestor's type through `wvMap{Stem}`.
            if eb.declared_here {
                render_error(&mut out, m, eb);
            }
        }
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
        for i in &m.interfaces {
            render_interface(&mut out, prefix, m, i, stem.as_deref());
        }
        for cb in &m.callbacks {
            render_callback_trampoline(&mut out, prefix, &m.path, cb);
        }
        for l in &m.listeners {
            render_listener_api(&mut out, m, l, strip_module_prefix);
        }
        for f in &m.functions {
            let go_name = wrapper_name(&m.path, &f.name, strip_module_prefix).to_upper_camel_case();
            let err = ErrCtx {
                throws: f.throws,
                stem: stem.as_deref(),
            };
            if let CallShape::Async(ab) = &f.shape {
                render_async_function(&mut out, prefix, &m.path, f, ab, &go_name, None, err);
            } else {
                render_function(&mut out, prefix, &m.path, f, &go_name, None, err);
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
/// (shared by all listeners firing it) and one per async completion callback,
/// including async interface members.
fn collect_trampoline_externs(model: &BindingModel, prefix: &str) -> Vec<String> {
    let mut decls = Vec::new();
    for m in &model.modules {
        for cb in &m.callbacks {
            decls.push(extern_decl(&cb.c_fn_type, &cb.abi_params, prefix));
        }
        for f in m.callables() {
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
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) | TypeRef::Interface(name) => {
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
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) | TypeRef::Interface(name) => {
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

/// The register/unregister wrapper pair for one listener. The wrapper names
/// follow the module-prefix-stripping default like free functions
/// (`RegisterEvictionListener` rather than `KvRegisterEvictionListener`).
fn render_listener_api(
    out: &mut String,
    m: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = m.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_go = wrapper_name(
        &m.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let unregister_go = wrapper_name(
        &m.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
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

/// The per-async-function outcome payload type name, derived from the
/// (unique) base C symbol with the ABI prefix dropped: free function
/// `weaveffi_io_read` names `wvOutcomeIoRead`, interface member
/// `weaveffi_kv_Store_compact` names `wvOutcomeKvStoreCompact`.
fn async_outcome_type(prefix: &str, f: &FnBinding) -> String {
    let base = f
        .c_base
        .strip_prefix(&format!("{prefix}_"))
        .unwrap_or(&f.c_base);
    format!("wvOutcome{}", base.to_upper_camel_case())
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
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) | TypeRef::Interface(n) => {
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
            TypeRef::Struct(n) | TypeRef::TypedHandle(n) | TypeRef::Interface(n) => {
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

/// An async callable: a blocking Go wrapper that launches the C call with a
/// completion trampoline and waits on a buffered channel, plus the outcome
/// type and the exported trampoline itself.
///
/// A throwing wrapper returns `(T, error)` and the trampoline maps a reported
/// error through the domain (`wvMap{Stem}`); a plain wrapper returns bare `T`
/// and panics on the calling goroutine when the outcome carries an error
/// (the trampoline itself must never panic: it runs on a producer thread
/// entered from C). With `receiver` set, the wrapper is a method on that
/// wrapper type passing `s.ptr` as the leading launch argument.
#[allow(clippy::too_many_arguments)]
fn render_async_function(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    ab: &AsyncBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let outcome = async_outcome_type(prefix, f);
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

    // The exported completion trampoline. It always converts a reported error
    // into a Go error and sends it over the channel; the wrapper decides
    // whether to return or panic with it.
    let formals: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", s.name, cgo_slot_type(&s.ty, prefix)))
        .collect();
    let mut tramp_body = String::new();
    emit_async_result_send(&mut tramp_body, &f.ret, &outcome, prefix, module);
    let map_err = err.map_call("wvTakeError(err)");
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            w.line("v := wvCallbackTake(uint64(uintptr(context)))");
            w.block("if v == nil {", "}", |w| {
                w.line("return");
            });
            w.line(format!("ch := v.(chan {outcome})"));
            w.block("if err != nil && err.code != 0 {", "}", |w| {
                w.line(format!("ch <- {outcome}{{err: {map_err}}}"));
                w.line("return");
            });
            w.raw(tramp_body.as_str());
        },
    );
    w.blank();

    // The blocking wrapper. Cancellation tokens are not surfaced (NULL).
    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();
    let ret_sig = err.ret_sig(&f.ret);
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    w.raw(doc);
    w.line("// Blocks until the async producer completes.");
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    if receiver.is_some() {
        c_args.push("s.ptr".into());
    }
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

    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}){ret_sig} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}){ret_sig} {{", go_params.join(", ")),
    };
    w.block(header, "}", |w| {
        w.line(format!("ch := make(chan {outcome}, 1)"));
        w.line("ctxID := wvCallbackStore(ch)");
        w.raw(pre.as_str());
        w.line(format!("C.{}({})", ab.launch.symbol, launch_args));
        w.line("outcome := <-ch");
        if err.throws {
            if let Some(ret) = &f.ret {
                w.block("if outcome.err != nil {", "}", |w| {
                    w.line(format!("return {}, outcome.err", go_zero(ret)));
                });
                w.line("return outcome.val, nil");
            } else {
                w.line("return outcome.err");
            }
        } else {
            w.block("if outcome.err != nil {", "}", |w| {
                w.line("panic(outcome.err)");
            });
            if f.ret.is_some() {
                w.line("return outcome.val");
            }
        }
    });
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

    // Variant construction is fallible plumbing (marshalling, producer
    // allocation), not a typed domain failure, so it reports the generic
    // brand error.
    let err = ErrCtx {
        throws: true,
        stem: None,
    };
    w.block(
        format!(
            "func {ctor}({}) (*{enum_name}, error) {{",
            go_params.join(", ")
        ),
        "}",
        |w| {
            w.raw(&pre);
            w.line(format!(
                "result := C.{}({})",
                v.create.symbol,
                c_args.join(", ")
            ));
            err.emit_check(w, "cErr", Some("nil"));
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
    // Build failures (missing required fields, marshalling) are plumbing
    // errors, not typed domain failures, so they report the generic brand
    // error.
    let err = ErrCtx {
        throws: true,
        stem: None,
    };
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
            err.emit_check(w, "cErr", Some("nil"));
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
            TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
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
                TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
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

// ── Interfaces ──

/// Render one interface as an opaque-object wrapper following the struct
/// pattern: a struct owning the `*C.{c_tag}` handle, freed by an explicit
/// `Close` (idempotent, nils the pointer).
///
/// Constructors become package-level factory functions named
/// `{PascalCtor}{Type}` (`new` gives `NewStore`, `open` gives `OpenStore`);
/// methods are methods on the wrapper passing `s.ptr` as the leading C
/// argument; statics are package-level functions namespaced by the type
/// (`StoreDefaultCapacity`). Members reuse the free-function marshalling
/// paths, including the sync/async/iterator shapes and the throws split.
fn render_interface(
    out: &mut String,
    prefix: &str,
    m: &ModuleBinding,
    iface: &InterfaceBinding,
    stem: Option<&str>,
) {
    let name = local_type_name(&iface.name).to_upper_camel_case();
    let c_tag = &iface.c_tag;

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &iface.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line(format!("ptr *C.{c_tag}"));
    });
    w.blank();
    out.push_str(&w.finish());

    for c in &iface.constructors {
        let go_name = format!("{}{name}", c.name.to_upper_camel_case());
        let err = ErrCtx {
            throws: c.throws,
            stem,
        };
        render_function(out, prefix, &m.path, c, &go_name, None, err);
    }

    for f in &iface.methods {
        let go_name = f.name.to_upper_camel_case();
        let err = ErrCtx {
            throws: f.throws,
            stem,
        };
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, &m.path, f, ab, &go_name, Some(&name), err);
        } else {
            render_function(out, prefix, &m.path, f, &go_name, Some(&name), err);
        }
    }

    for f in &iface.statics {
        let go_name = format!("{name}{}", f.name.to_upper_camel_case());
        let err = ErrCtx {
            throws: f.throws,
            stem,
        };
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, &m.path, f, ab, &go_name, None, err);
        } else {
            render_function(out, prefix, &m.path, f, &go_name, None, err);
        }
    }

    let mut w = CodeWriter::tabs();
    w.block(format!("func (s *{name}) Close() {{"), "}", |w| {
        w.block("if s.ptr != nil {", "}", |w| {
            w.line(format!("C.{}(s.ptr)", iface.destroy_symbol));
            w.line("s.ptr = nil");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

// ── Functions ──

/// A sync or iterator callable: the Go wrapper marshalling parameters in,
/// invoking the C symbol, checking the error slot per `err` (typed
/// `(T, error)` when throwing, `wvTrap` panic when plain), and converting the
/// result out. With `receiver` set, the wrapper is a method on that wrapper
/// type passing `s.ptr` as the leading C argument.
#[allow(clippy::too_many_arguments)]
fn render_function(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let c_sym = &f.c_base;

    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();

    let ret_sig = err.ret_sig(&f.ret);
    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}){ret_sig} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}){ret_sig} {{", go_params.join(", ")),
    };

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    w.raw(doc);
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    if receiver.is_some() {
        c_args.push("s.ptr".into());
    }

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
        emit_iterator_body(&mut body, &mut pre, &mut c_args, ib, prefix, module, err);
        w.line(header);
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

    w.block(header, "}", |w| {
        w.raw(pre.as_str());

        if f.ret.is_some() && !c_returns_void {
            w.line(format!("result := C.{c_sym}({args})"));
        } else {
            w.line(format!("C.{c_sym}({args})"));
        }

        err.emit_check(w, "cErr", f.ret.as_ref().map(go_zero).as_deref());

        if let Some(ref ret) = f.ret {
            let mut tail = String::new();
            emit_return(&mut tail, ret, prefix, module, err.ok_tail());
            w.raw(tail);
        } else if err.throws {
            w.line("return nil");
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Go type of the `out_item` local whose address is passed to an iterator's
/// `next` (the C slot is `T*`, so the local is one indirection less).
fn iter_out_item_type(inner: &TypeRef, prefix: &str, module: &str) -> String {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "*C.char".into(),
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
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
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
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
/// arguments (before `out_err`). Both the launch's error slot and each
/// `next`'s are checked per `err`: a throwing wrapper returns
/// `(nil, mapped)`, a plain one traps and keeps draining.
#[allow(clippy::too_many_arguments)]
fn emit_iterator_body(
    out: &mut String,
    pre: &mut String,
    c_args: &mut Vec<String>,
    ib: &weaveffi_core::model::IteratorBinding,
    prefix: &str,
    module: &str,
    err: ErrCtx,
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
    err.emit_check(&mut w, "cErr", Some("nil"));
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
        err.emit_check(w, "iterErr", Some("nil"));
        let mut app = String::new();
        emit_iter_elem_append(&mut app, "goResult", elem, "outItem");
        w.raw(app);
    });
    w.line(format!("return goResult{}", err.ok_tail()));
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
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_) => {
            args.push(format!("{name}.ptr"))
        }

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
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
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
    } else if let TypeRef::Struct(n) | TypeRef::TypedHandle(n) | TypeRef::Interface(n) = inner {
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

/// Emit the success-path return conversion. `tail` is [`ErrCtx::ok_tail`]:
/// `", nil"` when the wrapper also returns an error, empty when plain.
fn emit_return(out: &mut String, ty: &TypeRef, prefix: &str, module: &str, tail: &str) {
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
            w.line(format!("return {conv}{tail}"));
        }
        TypeRef::Bool => {
            w.line(format!("return cToBool(result){tail}"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("goResult := C.GoString(result)");
            w.line("C.weaveffi_free_string(result)");
            w.line(format!("return goResult{tail}"));
        }
        TypeRef::Enum(_) => {
            let conv = go_scalar_conv("result", ty);
            w.line(format!("return {conv}{tail}"));
        }
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("return &{g}{{ptr: result}}{tail}"));
        }
        TypeRef::Optional(inner) => return emit_optional_return(out, inner, module, tail),
        TypeRef::List(inner) => return emit_list_return(out, inner, prefix, module, tail),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.block("if result == nil {", "}", |w| {
                w.line(format!("return nil{tail}"));
            });
            w.line("goResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))");
            w.line("C.weaveffi_free_bytes(result, cOutLen)");
            w.line(format!("return goResult{tail}"));
        }
        TypeRef::Map(k, v) => return emit_map_return(out, k, v, prefix, module, tail),
        TypeRef::Iterator(inner) => return emit_list_return(out, inner, prefix, module, tail),
    }
    out.push_str(&w.finish());
}

fn emit_optional_return(out: &mut String, inner: &TypeRef, _module: &str, tail: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    w.block("if result == nil {", "}", |w| {
        w.line(format!("return nil{tail}"));
    });
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("v := C.GoString(result)");
            w.line("C.weaveffi_free_string(result)");
            w.line(format!("return &v{tail}"));
        }
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            w.line(format!("return &{g}{{ptr: result}}{tail}"));
        }
        TypeRef::Bool => {
            w.line("v := cToBool(*result)");
            w.line(format!("return &v{tail}"));
        }
        _ => {
            let gt = go_type(inner);
            w.line(format!("v := {gt}(*result)"));
            w.line(format!("return &v{tail}"));
        }
    }
    out.push_str(&w.finish());
}

fn emit_list_return(out: &mut String, inner: &TypeRef, prefix: &str, module: &str, tail: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    w.line("count := int(cOutLen)");
    let mut body = String::new();
    decode_list(
        &mut body, "goResult", inner, "result", "count", prefix, module,
    );
    w.raw(body);
    w.line(format!("return goResult{tail}"));
    out.push_str(&w.finish());
}

fn emit_map_return(
    out: &mut String,
    k: &TypeRef,
    v: &TypeRef,
    prefix: &str,
    module: &str,
    tail: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    w.line("count := int(cOutLen)");
    let mut body = String::new();
    decode_map(
        &mut body, "goResult", k, v, "cMapKeys", "cMapVals", "count", prefix, module,
    );
    w.raw(body);
    w.line(format!("return goResult{tail}"));
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
        } else if let TypeRef::TypedHandle(n) | TypeRef::Struct(n) | TypeRef::Interface(n) = inner {
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
    use weaveffi_ir::ir::{
        Api, CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef,
        ListenerDef, Module, Param, StructDef, StructField, TypeRef,
    };

    // ── Fixture helpers ──

    fn api_of(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn func_of(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn throwing(mut f: Function) -> Function {
        f.throws = true;
        f
    }

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
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

    fn code(name: &str, value: i32, message: &str) -> ErrorCode {
        ErrorCode {
            name: name.into(),
            code: value,
            message: message.into(),
            doc: None,
        }
    }

    /// Render with the default surface: `weaveffi` prefix, stripping on.
    fn rg(api: &Api) -> String {
        rg_with(api, "weaveffi", true)
    }

    fn rg_with(api: &Api, prefix: &str, strip: bool) -> String {
        let model = BindingModel::build(api, prefix);
        render_go(api, &model, prefix, strip, "weaveffi.yml")
    }

    fn calculator_api() -> Api {
        let mut m = module("calculator");
        m.functions = vec![
            func_of(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
            ),
            func_of(
                "echo",
                vec![param("msg", TypeRef::StringUtf8)],
                Some(TypeRef::StringUtf8),
            ),
        ];
        api_of(vec![m])
    }

    /// Mirrors `samples/kvstore/kvstore.yml`: the `Store` interface (ctor,
    /// sync/async/iterator methods, a static), the `KvError` domain, the
    /// `Entry` builder record, the eviction listener, and the nested
    /// `kv.stats` submodule taking a cross-module interface parameter.
    fn kv_api() -> Api {
        let mut stats = module("stats");
        stats.structs = vec![StructDef {
            name: "Stats".into(),
            doc: None,
            builder: false,
            fields: vec![field("total_entries", TypeRef::I64)],
        }];
        stats.functions = vec![throwing(func_of(
            "get_stats",
            // Cross-module references reach generators pre-qualified by the
            // validator's resolve step; mirror that spelling here.
            vec![param("store", TypeRef::Interface("kv.Store".into()))],
            Some(TypeRef::Struct("Stats".into())),
        ))];

        let mut kv = module("kv");
        kv.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                code("KeyNotFound", 1001, "key not found"),
                code("Expired", 1002, "entry expired"),
                code("StoreFull", 1003, "store has reached capacity"),
                code("IoError", 1004, "I/O failure"),
            ],
        });
        kv.structs = vec![StructDef {
            name: "Entry".into(),
            doc: None,
            builder: true,
            fields: vec![
                field("id", TypeRef::I64),
                field("key", TypeRef::StringUtf8),
                field("value", TypeRef::Bytes),
                field("expires_at", TypeRef::Optional(Box::new(TypeRef::I64))),
                field("tags", TypeRef::List(Box::new(TypeRef::StringUtf8))),
            ],
        }];
        kv.enums = vec![EnumDef {
            name: "EntryKind".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Volatile".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Persistent".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }];
        kv.callbacks = vec![CallbackDef {
            name: "OnEvict".into(),
            doc: None,
            params: vec![param("key", TypeRef::StringUtf8)],
        }];
        kv.listeners = vec![ListenerDef {
            name: "eviction_listener".into(),
            event_callback: "OnEvict".into(),
            doc: None,
        }];
        kv.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("An embedded key-value store owning its entries".into()),
            constructors: vec![throwing(func_of(
                "open",
                vec![param("path", TypeRef::StringUtf8)],
                None,
            ))],
            methods: vec![
                throwing(func_of(
                    "put",
                    vec![
                        param("key", TypeRef::StringUtf8),
                        param("value", TypeRef::Bytes),
                        param("kind", TypeRef::Enum("EntryKind".into())),
                        param("ttl_seconds", TypeRef::Optional(Box::new(TypeRef::I64))),
                    ],
                    Some(TypeRef::Bool),
                )),
                throwing(func_of(
                    "get",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::Optional(Box::new(TypeRef::Struct("Entry".into())))),
                )),
                throwing(func_of(
                    "list_keys",
                    vec![param(
                        "prefix",
                        TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    )],
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                )),
                func_of("count", vec![], Some(TypeRef::I64)),
                func_of("clear", vec![], None),
                {
                    let mut f = throwing(func_of("compact", vec![], Some(TypeRef::I64)));
                    f.r#async = true;
                    f.cancellable = true;
                    f
                },
                {
                    let mut f = throwing(func_of(
                        "legacy_put",
                        vec![
                            param("key", TypeRef::StringUtf8),
                            param("value", TypeRef::Bytes),
                        ],
                        Some(TypeRef::Bool),
                    ));
                    f.deprecated = Some("use put() with explicit kind".into());
                    f
                },
            ],
            statics: vec![func_of("default_capacity", vec![], Some(TypeRef::I64))],
        }];
        kv.modules = vec![stats];
        api_of(vec![kv])
    }

    /// Mirrors `samples/contacts/contacts.yml`, standing in for the CLI test
    /// (`cli_go.rs`) while the workspace binary is blocked on other generator
    /// crates mid-overhaul.
    fn contacts_api() -> Api {
        let mut m = module("contacts");
        m.enums = vec![EnumDef {
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
        }];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![
                field("id", TypeRef::I64),
                field("first_name", TypeRef::StringUtf8),
                field("last_name", TypeRef::StringUtf8),
                field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                field("contact_type", TypeRef::Enum("ContactType".into())),
            ],
        }];
        m.errors = Some(ErrorDomain {
            name: "ContactsError".into(),
            codes: vec![
                code("InvalidName", 1, "name must not be empty"),
                code("NotFound", 2, "contact not found"),
            ],
        });
        m.interfaces = vec![InterfaceDef {
            name: "ContactBook".into(),
            doc: Some("An in-memory address book owning its contacts".into()),
            constructors: vec![func_of("new", vec![], None)],
            methods: vec![
                throwing(func_of(
                    "add",
                    vec![
                        param("first_name", TypeRef::StringUtf8),
                        param("last_name", TypeRef::StringUtf8),
                        param("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                        param("contact_type", TypeRef::Enum("ContactType".into())),
                    ],
                    Some(TypeRef::Struct("Contact".into())),
                )),
                throwing(func_of(
                    "get",
                    vec![param("id", TypeRef::I64)],
                    Some(TypeRef::Struct("Contact".into())),
                )),
                func_of(
                    "list",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                ),
                func_of(
                    "remove",
                    vec![param("id", TypeRef::I64)],
                    Some(TypeRef::Bool),
                ),
                func_of("count", vec![], Some(TypeRef::I32)),
            ],
            statics: vec![],
        }];
        api_of(vec![m])
    }

    // ── Scaffolding and packaging ──

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
        let go = rg(&calculator_api());
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

    #[test]
    fn imports_fmt_and_unsafe() {
        let go = rg(&calculator_api());
        assert!(go.contains("\"fmt\""), "missing fmt import: {go}");
        assert!(go.contains("\"unsafe\""), "missing unsafe import: {go}");
    }

    // ── Plain (non-throwing) functions ──

    #[test]
    fn simple_i32_function() {
        let go = rg(&calculator_api());
        assert!(
            go.contains("func Add(a int32, b int32) int32 {"),
            "missing plain function sig: {go}"
        );
        assert!(
            go.contains("C.weaveffi_calculator_add("),
            "missing C call: {go}"
        );
        assert!(go.contains("C.int32_t(a)"), "missing param cast: {go}");
        assert!(go.contains("return int32(result)"), "missing return: {go}");
        assert!(
            !go.contains("return int32(result), nil"),
            "plain function must not return an error: {go}"
        );
    }

    #[test]
    fn string_function() {
        let go = rg(&calculator_api());
        assert!(
            go.contains("func Echo(msg string) string {"),
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
    fn plain_function_traps_on_error() {
        let go = rg(&calculator_api());
        assert!(
            go.contains("var cErr C.weaveffi_error"),
            "missing error var: {go}"
        );
        assert!(go.contains("wvTrap(&cErr)"), "missing trap check: {go}");
        assert!(
            go.contains("func wvTrap(cErr *C.weaveffi_error) {"),
            "missing wvTrap helper: {go}"
        );
        assert!(
            go.contains("C.weaveffi_error_clear(cErr)"),
            "missing error clear in wvTakeError: {go}"
        );
        assert!(
            go.contains("panic(fmt.Sprintf(\"weaveffi: %s (code %d)\", msg, code))"),
            "wvTrap must panic: {go}"
        );
    }

    #[test]
    fn void_function() {
        let mut m = module("system");
        m.functions = vec![func_of("reset", vec![], None)];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Reset() {"),
            "missing plain void sig: {go}"
        );
        assert!(
            go.contains("wvTrap(&cErr)"),
            "plain void must trap on error: {go}"
        );
        assert!(
            !go.contains("func Reset() error"),
            "plain void must not return error: {go}"
        );
    }

    #[test]
    fn handle_type() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "create",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::Handle),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Create(name string) int64 {"),
            "handle return should be plain int64: {go}"
        );
        assert!(
            go.contains("return int64(result)"),
            "missing handle return conversion: {go}"
        );
    }

    #[test]
    fn bool_function_generates_helpers() {
        let mut m = module("logic");
        m.functions = vec![func_of(
            "negate",
            vec![param("val", TypeRef::Bool)],
            Some(TypeRef::Bool),
        )];
        let go = rg(&api_of(vec![m]));
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
        let mut m = module("paint");
        m.functions = vec![func_of(
            "mix",
            vec![param("a", TypeRef::Enum("Color".into()))],
            Some(TypeRef::Enum("Color".into())),
        )];
        m.enums = vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![EnumVariant {
                name: "Red".into(),
                value: 0,
                doc: None,
                fields: vec![],
            }],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Mix(a Color) Color {"),
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
        let mut m = module("contacts");
        m.functions = vec![func_of(
            "get_contact",
            vec![param("id", TypeRef::Handle)],
            Some(TypeRef::Struct("Contact".into())),
        )];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func GetContact(id int64) *Contact {"),
            "plain struct return should be bare *Contact: {go}"
        );
        assert!(
            go.contains("return &Contact{ptr: result}"),
            "missing struct wrap: {go}"
        );
    }

    #[test]
    fn optional_string_param() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "find",
            vec![param(
                "query",
                TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
            )],
            None,
        )];
        let go = rg(&api_of(vec![m]));
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
        let mut m = module("contacts");
        m.functions = vec![func_of(
            "find",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into(),
            )))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Find(id int32) *Contact {"),
            "optional struct return: {go}"
        );
        assert!(go.contains("if result == nil"), "missing nil check: {go}");
    }

    #[test]
    fn list_return() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "list_ids",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::I32))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func ListIds() []int32 {"),
            "missing plain list return sig: {go}"
        );
        assert!(
            go.contains("var cOutLen C.size_t"),
            "missing out_len var: {go}"
        );
        assert!(go.contains("unsafe.Slice("), "missing unsafe.Slice: {go}");
    }

    #[test]
    fn struct_list_return() {
        let mut m = module("contacts");
        m.functions = vec![func_of(
            "list_contacts",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
        )];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func ListContacts() []*Contact {"),
            "missing struct list return: {go}"
        );
        assert!(
            go.contains("C.weaveffi_contacts_Contact"),
            "missing C struct type in list conversion: {go}"
        );
    }

    #[test]
    fn optional_i32_param() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "find",
            vec![param("id", TypeRef::Optional(Box::new(TypeRef::I32)))],
            Some(TypeRef::Optional(Box::new(TypeRef::I32))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("id *int32"),
            "optional i32 param should be *int32: {go}"
        );
        assert!(
            go.contains("var cId *C.int32_t"),
            "missing C var for optional: {go}"
        );
    }

    // ── Throwing functions ──

    fn store_api() -> Api {
        let mut m = module("store");
        m.errors = Some(ErrorDomain {
            name: "StoreError".into(),
            codes: vec![code("SaveFailed", 1, "save failed")],
        });
        m.functions = vec![
            throwing(func_of(
                "save",
                vec![param("data", TypeRef::StringUtf8)],
                Some(TypeRef::I32),
            )),
            throwing(func_of("flush", vec![], None)),
            func_of("clear", vec![], None),
        ];
        api_of(vec![m])
    }

    #[test]
    fn throws_split_sync() {
        let go = rg(&store_api());
        // throws == true keeps `(T, error)` and maps through the domain.
        assert!(
            go.contains("func Save(data string) (int32, error) {"),
            "missing throwing sig: {go}"
        );
        assert!(
            go.contains("if cErr.code != 0 {"),
            "missing error check: {go}"
        );
        assert!(
            go.contains("return 0, wvMapStore(wvTakeError(&cErr))"),
            "throwing wrapper must map the domain error: {go}"
        );
        assert!(
            go.contains("return int32(result), nil"),
            "throwing wrapper must return `, nil` on success: {go}"
        );
        // Throwing void: `error` result, nil on success.
        assert!(
            go.contains("func Flush() error {"),
            "missing throwing void sig: {go}"
        );
        assert!(
            go.contains("return wvMapStore(wvTakeError(&cErr))"),
            "throwing void must return the mapped error: {go}"
        );
        assert!(go.contains("return nil"), "missing nil return: {go}");
        // throws == false stays plain and traps.
        assert!(
            go.contains("func Clear() {"),
            "missing plain void sig: {go}"
        );
        assert!(go.contains("wvTrap(&cErr)"), "missing trap: {go}");
    }

    #[test]
    fn typed_error_surface() {
        let go = rg(&store_api());
        assert!(
            go.contains("type StoreError struct {"),
            "missing typed error struct: {go}"
        );
        assert!(
            go.contains("func (e *StoreError) Error() string {"),
            "typed error must implement error: {go}"
        );
        assert!(
            go.contains("StoreErrorSaveFailed int32 = 1"),
            "missing exported code constant: {go}"
        );
        assert!(
            go.contains("func wvMapStore(code int32, message string) error {"),
            "missing domain mapping helper: {go}"
        );
        assert!(
            go.contains("message = \"save failed\""),
            "missing default message fill: {go}"
        );
        assert!(
            go.contains("return wvBrandError(code, message)"),
            "unknown codes must fall back to the brand error: {go}"
        );
        assert!(
            go.contains(&format!("type {ERROR_BRAND} struct {{")),
            "missing generic brand error: {go}"
        );
    }

    // ── Enums, structs, builders ──

    #[test]
    fn enum_generation() {
        let mut m = module("paint");
        m.enums = vec![EnumDef {
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
        }];
        let go = rg(&api_of(vec![m]));
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
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
            ],
        }];
        let go = rg(&api_of(vec![m]));
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
        let mut m = module("geo");
        m.structs = vec![StructDef {
            name: "Point".into(),
            doc: None,
            builder: true,
            fields: vec![field("x", TypeRef::F64)],
        }];
        let go = rg(&api_of(vec![m]));
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
        assert!(
            go.contains("return nil, wvBrandError(wvTakeError(&cErr))"),
            "Build failures are brand errors: {go}"
        );
    }

    #[test]
    fn struct_optional_string_field() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![field(
                "email",
                TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
            )],
        }];
        let go = rg(&api_of(vec![m]));
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
    fn struct_enum_field_getter() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![field("contact_type", TypeRef::Enum("ContactType".into()))],
        }];
        m.enums = vec![EnumDef {
            name: "ContactType".into(),
            doc: None,
            variants: vec![EnumVariant {
                name: "Personal".into(),
                value: 0,
                doc: None,
                fields: vec![],
            }],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func (s *Contact) ContactType() ContactType"),
            "missing enum field getter: {go}"
        );
    }

    #[test]
    fn no_bool_helpers_when_unneeded() {
        let mut m = module("math");
        m.functions = vec![func_of(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            !go.contains("boolToC"),
            "should not include bool helpers: {go}"
        );
    }

    // ── Async ──

    /// Async functions get a blocking wrapper: a registry-id context, an
    /// exported completion trampoline, and a buffered channel the wrapper
    /// waits on. The channel is buffered so the producer thread never blocks
    /// on the send even if the waiter has already given up.
    #[test]
    fn go_async_generates_blocking_wrapper() {
        let mut m = module("io");
        m.functions = vec![
            {
                let mut f = func_of("read", vec![], Some(TypeRef::StringUtf8));
                f.r#async = true;
                f
            },
            func_of("write", vec![], None),
        ];
        let go = rg(&api_of(vec![m]));
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
            go.contains("func Read() string {"),
            "plain async wrapper must have a bare return: {go}"
        );
        assert!(
            go.contains("ch := make(chan wvOutcomeIoRead, 1)"),
            "wrapper must wait on a buffered outcome channel: {go}"
        );
        assert!(
            go.contains("panic(outcome.err)"),
            "plain async wrapper must panic on a reported error: {go}"
        );
        assert!(
            go.contains("return outcome.val"),
            "plain async wrapper returns the outcome value: {go}"
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
    fn async_cancellable_passes_null_token() {
        let mut m = module("tasks");
        m.functions = vec![{
            let mut f = func_of("run", vec![], Some(TypeRef::I32));
            f.r#async = true;
            f.cancellable = true;
            f
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Run() int32 {"),
            "async wrapper must be generated: {go}"
        );
        assert!(
            go.contains("C.weaveffi_tasks_run_async(nil, "),
            "cancel token must be passed as NULL: {go}"
        );
    }

    // ── Listeners ──

    #[test]
    fn listeners_generate_register_unregister() {
        let mut m = module("events");
        m.callbacks = vec![CallbackDef {
            name: "OnMessage".into(),
            doc: None,
            params: vec![param("message", TypeRef::StringUtf8)],
        }];
        m.listeners = vec![ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }];
        let go = rg(&api_of(vec![m]));
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
            go.contains("func RegisterMessageListener(callback func(message string)) uint64 {"),
            "register wrapper must be emitted with the stripped name: {go}"
        );
        assert!(
            go.contains("func UnregisterMessageListener(id uint64) {"),
            "unregister wrapper must be emitted with the stripped name: {go}"
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

    // ── Interfaces ──

    #[test]
    fn interface_wrapper_and_ctor() {
        let go = rg(&kv_api());
        assert!(
            go.contains("type Store struct {"),
            "missing interface wrapper struct: {go}"
        );
        assert!(
            go.contains("ptr *C.weaveffi_kv_Store"),
            "missing wrapped C pointer: {go}"
        );
        // Factory constructor: `open` -> `OpenStore`, throwing.
        assert!(
            go.contains("func OpenStore(path string) (*Store, error) {"),
            "missing factory constructor: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_kv_Store_open(cPath, &cErr)"),
            "ctor must call the member symbol: {go}"
        );
        assert!(
            go.contains("return nil, wvMapKv(wvTakeError(&cErr))"),
            "throwing ctor maps the domain error: {go}"
        );
        assert!(
            go.contains("return &Store{ptr: result}, nil"),
            "ctor wraps the owned pointer: {go}"
        );
    }

    #[test]
    fn interface_new_ctor_naming() {
        let go = rg(&contacts_api());
        assert!(
            go.contains("func NewContactBook() *ContactBook {"),
            "ctor named `new` must surface as New<Type>: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_contacts_ContactBook_new(&cErr)"),
            "missing ctor symbol call: {go}"
        );
        assert!(
            go.contains("return &ContactBook{ptr: result}"),
            "plain ctor wraps without error: {go}"
        );
    }

    #[test]
    fn interface_methods_pass_self() {
        let go = rg(&kv_api());
        // Throwing method: `(T, error)` with the receiver's ptr leading.
        assert!(
            go.contains(
                "func (s *Store) Put(key string, value []byte, kind EntryKind, ttlSeconds *int64) (bool, error) {"
            ),
            "missing throwing method: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_kv_Store_put(s.ptr, cKey, cValuePtr, cValueLen, C.weaveffi_kv_EntryKind(kind), cTtlSeconds, &cErr)"),
            "method must pass s.ptr as the leading C argument: {go}"
        );
        assert!(
            go.contains("return false, wvMapKv(wvTakeError(&cErr))"),
            "throwing bool method returns its zero value with the error: {go}"
        );
        // Optional struct return through a method.
        assert!(
            go.contains("func (s *Store) Get(key string) (*Entry, error) {"),
            "missing optional-return method: {go}"
        );
        // Plain method: bare return, traps.
        assert!(
            go.contains("func (s *Store) Count() int64 {"),
            "missing plain method: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_kv_Store_count(s.ptr, &cErr)"),
            "plain method must pass s.ptr: {go}"
        );
        // Plain void method.
        assert!(
            go.contains("func (s *Store) Clear() {"),
            "missing plain void method: {go}"
        );
        // Deprecated member keeps its notice.
        assert!(
            go.contains("// Deprecated: use put() with explicit kind"),
            "missing deprecation notice: {go}"
        );
    }

    #[test]
    fn interface_static_naming() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func StoreDefaultCapacity() int64 {"),
            "statics are package-level, namespaced by the type: {go}"
        );
        assert!(
            go.contains("C.weaveffi_kv_Store_default_capacity(&cErr)"),
            "static must call the member symbol without self: {go}"
        );
    }

    #[test]
    fn interface_close_calls_destroy() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func (s *Store) Close() {"),
            "missing Close: {go}"
        );
        assert!(
            go.contains("C.weaveffi_kv_Store_destroy(s.ptr)"),
            "Close must call the destroy symbol: {go}"
        );
    }

    #[test]
    fn interface_async_method_throws() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func (s *Store) Compact() (int64, error) {"),
            "async throwing method keeps (T, error): {go}"
        );
        assert!(
            go.contains("type wvOutcomeKvStoreCompact struct {"),
            "outcome type derives from the member symbol: {go}"
        );
        assert!(
            go.contains("//export goWv_weaveffi_kv_Store_compact_callback"),
            "member trampoline must be exported: {go}"
        );
        assert!(
            go.contains("C.weaveffi_kv_Store_compact_async(s.ptr, nil, "),
            "launch passes s.ptr then the NULL cancel token: {go}"
        );
        assert!(
            go.contains("ch <- wvOutcomeKvStoreCompact{err: wvMapKv(wvTakeError(err))}"),
            "trampoline maps the domain error: {go}"
        );
        assert!(
            go.contains("return 0, outcome.err"),
            "throwing async wrapper returns the outcome error: {go}"
        );
    }

    #[test]
    fn interface_iterator_method_throws() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func (s *Store) ListKeys(prefix *string) ([]string, error) {"),
            "throwing iterator method keeps ([]T, error): {go}"
        );
        assert!(
            go.contains("it := C.weaveffi_kv_Store_list_keys(s.ptr, cPrefix, &cErr)"),
            "iterator launch passes s.ptr: {go}"
        );
        assert!(
            go.contains("defer C.weaveffi_kv_Store_ListKeysIterator_destroy(it)"),
            "iterator must be destroyed: {go}"
        );
        assert!(
            go.contains(
                "if C.weaveffi_kv_Store_ListKeysIterator_next(it, &outItem, &iterErr) == 0 {"
            ),
            "iterator must drain via next: {go}"
        );
        assert!(
            go.contains("return nil, wvMapKv(wvTakeError(&iterErr))"),
            "per-element errors map through the domain: {go}"
        );
        assert!(
            go.contains("return goResult, nil"),
            "successful drain returns `, nil`: {go}"
        );
    }

    #[test]
    fn plain_iterator_function_traps() {
        let mut m = module("events");
        m.functions = vec![func_of(
            "get_messages",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func GetMessages() []string {"),
            "plain iterator returns a bare slice: {go}"
        );
        assert!(
            go.contains("wvTrap(&iterErr)"),
            "plain iterator traps per-element errors: {go}"
        );
        assert!(
            !go.contains("func GetMessages() ([]string, error)"),
            "plain iterator must not return an error: {go}"
        );
    }

    #[test]
    fn cross_module_interface_param_borrows() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func GetStats(store *Store) (*Stats, error) {"),
            "nested-module function takes the wrapper: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_kv_stats_get_stats(store.ptr, &cErr)"),
            "interface params borrow the wrapped pointer: {go}"
        );
        assert!(
            go.contains("return nil, wvMapKv(wvTakeError(&cErr))"),
            "inheriting submodule maps through the ancestor domain: {go}"
        );
    }

    #[test]
    fn typed_error_emitted_once_with_all_codes() {
        let go = rg(&kv_api());
        assert_eq!(
            go.matches("type KvError struct {").count(),
            1,
            "domain type must be emitted exactly once: {go}"
        );
        assert!(go.contains("KvErrorKeyNotFound int32 = 1001"), "{go}");
        assert!(go.contains("KvErrorExpired int32 = 1002"), "{go}");
        assert!(go.contains("KvErrorStoreFull int32 = 1003"), "{go}");
        assert!(go.contains("KvErrorIoError int32 = 1004"), "{go}");
        assert!(
            go.contains("func wvMapKv(code int32, message string) error {"),
            "missing wvMapKv helper: {go}"
        );
        assert!(
            go.contains("case KvErrorKeyNotFound:"),
            "mapping must switch on the code constants: {go}"
        );
    }

    #[test]
    fn kv_listener_uses_stripped_names() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func RegisterEvictionListener(callback func(key string)) uint64 {"),
            "{go}"
        );
        assert!(
            go.contains("func UnregisterEvictionListener(id uint64) {"),
            "{go}"
        );
    }

    // ── Naming ──

    #[test]
    fn module_prefix_stripping_default_and_knob() {
        let api = calculator_api();
        let stripped = rg(&api);
        assert!(
            stripped.contains("func Add(a int32, b int32) int32 {"),
            "stripping is the default: {stripped}"
        );
        assert!(
            !stripped.contains("func CalculatorAdd("),
            "stripped output must not keep the module prefix: {stripped}"
        );
        let prefixed = rg_with(&api, "weaveffi", false);
        assert!(
            prefixed.contains("func CalculatorAdd(a int32, b int32) int32 {"),
            "knob off restores the module prefix: {prefixed}"
        );
    }

    #[test]
    fn nested_module_stripping() {
        let go = rg_with(&kv_api(), "weaveffi", false);
        assert!(
            go.contains("func KvStatsGetStats(store *Store)"),
            "unstripped nested-module functions carry the full path: {go}"
        );
        // Interface members are namespaced by their type, never the module.
        assert!(
            go.contains("func (s *Store) Put("),
            "interface members are unaffected by the knob: {go}"
        );
        assert!(
            go.contains("func OpenStore(path string)"),
            "constructors are unaffected by the knob: {go}"
        );
    }

    #[test]
    fn contacts_surface_matches_cli_expectations() {
        let go = rg(&contacts_api());
        assert!(go.contains("type ContactType int32"), "{go}");
        assert!(go.contains("type Contact struct {"), "{go}");
        assert!(go.contains("type ContactBook struct {"), "{go}");
        assert!(go.contains("ptr *C.weaveffi_contacts_ContactBook"), "{go}");
        assert!(
            go.contains("func (s *ContactBook) Add(firstName string, lastName string, email *string, contactType ContactType) (*Contact, error) {"),
            "{go}"
        );
        assert!(
            go.contains("func (s *ContactBook) Get(id int64) (*Contact, error) {"),
            "{go}"
        );
        assert!(
            go.contains("func (s *ContactBook) List() []*Contact {"),
            "{go}"
        );
        assert!(
            go.contains("func (s *ContactBook) Remove(id int64) bool {"),
            "{go}"
        );
        assert!(go.contains("func (s *ContactBook) Count() int32 {"), "{go}");
        assert!(go.contains("func (s *ContactBook) Close() {"), "{go}");
        assert!(
            go.contains("C.weaveffi_contacts_ContactBook_destroy(s.ptr)"),
            "{go}"
        );
        assert!(go.contains("type ContactsError struct {"), "{go}");
        assert!(go.contains("ContactsErrorInvalidName int32 = 1"), "{go}");
        assert!(go.contains("ContactsErrorNotFound int32 = 2"), "{go}");
        assert!(
            go.contains("func wvMapContacts(code int32, message string) error {"),
            "{go}"
        );
        assert!(
            go.contains("return nil, wvMapContacts(wvTakeError(&cErr))"),
            "{go}"
        );
    }

    // ── Generate-to-disk paths ──

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
            go.contains("func Add(a int32, b int32) int32 {"),
            "missing add function: {go}"
        );
        assert!(
            go.contains("func Echo(msg string) string {"),
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

    // ── Ordering and memory-safety details ──

    #[test]
    fn go_no_double_free_on_error() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func_of(
            "find_contact",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::Struct("Contact".into())),
        )];
        let go = rg(&api_of(vec![m]));

        let fn_start = go.find("func FindContact(").expect("FindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        assert!(
            !fn_text.contains("weaveffi_free_string(cName"),
            "borrowed string param must not be freed via weaveffi_free_string: {fn_text}"
        );

        let err_check = fn_text
            .find("wvTrap(&cErr)")
            .expect("trap check in FindContact");
        let contact_wrap = fn_text
            .find("&Contact{ptr: result}")
            .expect("Contact wrap in FindContact");
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
        let mut m = module("contacts");
        m.functions = vec![func_of(
            "find_contact",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into(),
            )))),
        )];
        let go = rg(&api_of(vec![m]));

        let fn_start = go.find("func FindContact(").expect("FindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        let null_check = fn_text
            .find("if result == nil")
            .expect("nil check in FindContact");
        let contact_wrap = fn_text
            .find("&Contact{ptr: result}")
            .expect("Contact wrap in FindContact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check nil before wrapping: {fn_text}"
        );
    }

    // ── Docs ──

    fn doc_api() -> Api {
        let mut m = module("docs");
        m.functions = vec![Function {
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
        }];
        m.structs = vec![StructDef {
            name: "Item".into(),
            doc: Some("An item we track.".into()),
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: Some("Stable id".into()),
                default: None,
            }],
            builder: false,
        }];
        m.enums = vec![EnumDef {
            name: "Kind".into(),
            doc: Some("Kind of item.".into()),
            variants: vec![EnumVariant {
                name: "Small".into(),
                value: 0,
                doc: Some("A small one".into()),
                fields: vec![],
            }],
        }];
        api_of(vec![m])
    }

    #[test]
    fn go_emits_doc_on_function() {
        let go = rg(&doc_api());
        assert!(go.contains("// DoThing: Performs a thing."), "{go}");
    }

    #[test]
    fn go_emits_doc_on_struct() {
        let go = rg(&doc_api());
        assert!(go.contains("// Item: An item we track."), "{go}");
    }

    #[test]
    fn go_emits_doc_on_enum_variant() {
        let go = rg(&doc_api());
        assert!(go.contains("// Kind: Kind of item."), "{go}");
        assert!(go.contains("// KindSmall: A small one"), "{go}");
    }

    #[test]
    fn go_emits_doc_on_field() {
        let go = rg(&doc_api());
        assert!(go.contains("// Id: Stable id"), "{go}");
    }

    #[test]
    fn go_emits_doc_on_param() {
        let go = rg(&doc_api());
        assert!(go.contains("// Parameters:"), "{go}");
        assert!(go.contains("//   - x: the input value"), "{go}");
    }

    #[test]
    fn go_custom_prefix_threads_to_user_symbols() {
        let go = rg_with(&calculator_api(), "myffi", true);
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
