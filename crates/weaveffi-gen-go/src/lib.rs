//! Go (CGo) binding generator for WeaveFFI.
//!
//! Emits a Go module (`go.mod` + package) with CGo bindings over the C
//! ABI exposed by the underlying cdylib. Implements [`LanguageBackend`];
//! the shared driver bridges it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps are value types that
//! cross the C ABI serialized in the WeaveFFI value-buffer format (one
//! `const uint8_t*` + `size_t` pair). The generated package carries a small
//! private writer/reader implementing the wire format, plus one pack and one
//! unpack function per record and rich enum.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::collections::HashSet;

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{is_buffered, AbiParam, CType, ConstPos};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::plan::{elem_free, ElemFree, ErrorStrategy};
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

/// The local Go type name (PascalCase) of a user-defined type reference,
/// stripping any qualifying module path.
fn go_local(n: &str) -> String {
    local_type_name(n).to_upper_camel_case()
}

/// The Go wrapper type name for a typed-handle referent: `{Name}Handle`.
/// The suffix keeps the wrapper distinct from the referent's value struct.
fn handle_wrapper(n: &str) -> String {
    format!("{}Handle", go_local(n))
}

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
        // Records are plain value structs; rich enums are sealed interfaces
        // (nil-able), so neither takes a pointer at the type site. A
        // cross-module reference (resolved to e.g. `kv.Entry`) must name the
        // local `Entry` type rather than the qualified `KvEntry`.
        TypeRef::Record(n) | TypeRef::RichEnum(n) => go_local(n),
        TypeRef::Interface(n) => format!("*{}", go_local(n)),
        TypeRef::TypedHandle(n) => format!("*{}", handle_wrapper(n)),
        TypeRef::Enum(n) => go_local(n),
        TypeRef::Optional(inner) => {
            if optional_derefs(inner) {
                format!("*{}", go_type(inner))
            } else {
                // Already nil-able in Go (interface, slice, map, byte slice,
                // handle wrapper): nil is the none marker.
                go_type(inner)
            }
        }
        TypeRef::List(inner) => format!("[]{}", go_type(inner)),
        // The bare (non-throwing) sequence type; a throwing iterator wrapper
        // spells `iter.Seq2[T, error]` at its signature site instead.
        TypeRef::Iterator(inner) => format!("iter.Seq[{}]", go_type(inner)),
        TypeRef::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// `true` when `T?` surfaces as `*T` in Go (the value must be dereferenced
/// when present). Types that are already nil-able (rich enums, slices, maps,
/// byte slices, typed handles, interfaces) use nil directly as the none
/// marker instead.
fn optional_derefs(inner: &TypeRef) -> bool {
    !matches!(
        inner,
        TypeRef::RichEnum(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_)
    )
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
        // A record is a value struct: its zero is the empty literal.
        TypeRef::Record(n) => format!("{}{{}}", go_local(n)),
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
        TypeRef::Enum(n) => format!("{}({expr})", go_local(n)),
        _ => expr.to_string(),
    }
}

/// The Go expression wrapping an opaque C pointer (`ptr_expr`) into the
/// wrapper type for an interface or typed-handle reference.
fn go_wrap_expr(ty: &TypeRef, ptr_expr: &str) -> String {
    match ty {
        TypeRef::Interface(n) => format!("&{}{{ptr: {ptr_expr}}}", go_local(n)),
        TypeRef::TypedHandle(n) => format!("&{}{{ptr: {ptr_expr}}}", handle_wrapper(n)),
        _ => unreachable!("only interfaces and typed handles wrap C pointers"),
    }
}

// ── Import scanning ──

/// `true` when `ty` is a bare bool in a returned position (including an
/// iterator element), needing the `cToBool` helper.
fn ret_direct_bool(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Bool => true,
        TypeRef::Iterator(inner) => matches!(inner.as_ref(), TypeRef::Bool),
        _ => false,
    }
}

/// What the generated file's preamble must pull in, computed by one pass over
/// the lowered model.
#[derive(Default, Clone, Copy)]
struct Imports {
    /// `fmt` (error formatting); implied by [`err_infra`](Self::err_infra).
    fmt: bool,
    /// `iter` (lazy sequences returned by `iter<T>` functions).
    iter: bool,
    /// `unsafe` (pointer staging for strings/bytes/buffers, callback
    /// contexts).
    unsafe_ptr: bool,
    /// The `boolToC`/`cToBool` helpers.
    bool_helpers: bool,
    /// `sync` (the callback registry mutex).
    sync: bool,
    /// The shared error plumbing: the [`ERROR_BRAND`] type plus the
    /// `wvTakeError`/`wvBrandError`/`wvTrap` helpers.
    err_infra: bool,
    /// The value-buffer runtime (`wvWriter`/`wvReader` and buffer copy
    /// helpers), pulling in `encoding/binary`, `math`, and `unicode/utf8`.
    buffer_runtime: bool,
}

/// Scan the lowered model for everything [`Imports`] tracks. Interface
/// members participate exactly like free functions (via
/// [`ModuleBinding::callables`]).
fn scan_imports(model: &BindingModel) -> Imports {
    let mut any_callable = false;
    let mut has_async = false;
    let mut has_iter = false;
    let mut has_listeners = false;
    let mut has_domain = false;
    let mut any_callbacks = false;
    let mut bool_helpers = false;
    let mut buffer_runtime = false;

    for m in &model.modules {
        has_listeners |= !m.listeners.is_empty();
        has_domain |= m.declares_error();
        // Records and rich enums always carry pack/unpack functions; a
        // declared domain with payload fields decodes them through a reader.
        buffer_runtime |= !m.structs.is_empty();
        buffer_runtime |= m.enums.iter().any(|e| e.is_rich());
        if let Some(eb) = &m.error {
            buffer_runtime |= eb.declared_here && eb.codes.iter().any(|c| !c.fields.is_empty());
        }
        for f in m.callables() {
            any_callable = true;
            has_async |= f.is_async;
            has_iter |= matches!(f.shape, CallShape::Iterator(_));
            buffer_runtime |= f.params.iter().any(|p| is_buffered(&p.ty));
            bool_helpers |= f.params.iter().any(|p| matches!(p.ty, TypeRef::Bool));
            if let Some(ret) = &f.ret {
                buffer_runtime |= is_buffered(ret);
                bool_helpers |= ret_direct_bool(ret);
            }
            if let CallShape::Iterator(ib) = &f.shape {
                // Bytes and buffered elements copy through wvCopyBuffer.
                buffer_runtime |= matches!(elem_free(&ib.elem), ElemFree::Bytes);
            }
        }
        for cb in &m.callbacks {
            any_callbacks = true;
            buffer_runtime |= cb.params.iter().any(|p| is_buffered(&p.ty));
            bool_helpers |= cb.params.iter().any(|p| matches!(p.ty, TypeRef::Bool));
        }
    }

    // Every callable checks its error slot (returning or trapping), so any
    // callable at all pulls in the error plumbing; a declared domain also
    // needs it for the brand-error fallback of its mapping helper.
    let err_infra = any_callable || has_domain;
    // wvTakeError copies the payload through unsafe.Pointer; the buffer
    // runtime copies C buffers; trampolines carry `void* context`.
    let unsafe_ptr = err_infra || buffer_runtime || any_callbacks || has_listeners || has_async;

    Imports {
        fmt: err_infra,
        iter: has_iter,
        unsafe_ptr,
        bool_helpers,
        sync: has_async || has_listeners,
        err_infra,
        buffer_runtime,
    }
}

// ── Packaging scaffold ──

fn render_go_mod(module_path: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let trailer = render_trailer(CommentStyle::DoubleSlash, "go.mod");
    // Go 1.23 is required for the standard `iter` package the lazy
    // `iter<T>` wrappers return.
    format!("{prelude}module {module_path}\n\ngo 1.23\n\n{trailer}")
}

fn render_readme(input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    format!(
        r#"{prelude}# WeaveFFI Go Bindings

Auto-generated Go bindings using CGo.

## Prerequisites

- Go >= 1.23 (the bindings return standard `iter` package sequences)
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
converts the result back to Go types. Records, rich enums, optionals, lists,
and maps cross the boundary serialized in the WeaveFFI value-buffer format.
Errors are returned as Go `error` values.

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
/// the generic [`ERROR_BRAND`] struct when no domain is in scope. A callable
/// with `throws == false` has a plain signature and panics via `wvTrap`
/// instead, since a reported error can only be a producer panic or an
/// argument-marshalling failure.
#[derive(Clone, Copy)]
struct ErrCtx<'a> {
    /// `true` when the wrapper returns `(T, error)` and surfaces typed errors.
    throws: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `wvMapKv`); `None`
    /// falls back to the generic `wvBrandError` constructor.
    stem: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// Build the wrapper error context for `f` from the shared plan's
    /// [`ErrorStrategy`]: a `Throws` callable returns `(T, error)` through
    /// `stem`'s typed domain, a `Trap` callable panics via `wvTrap`.
    fn of(f: &FnBinding, stem: Option<&'a str>) -> Self {
        Self {
            throws: matches!(f.error_strategy(), ErrorStrategy::Throws),
            stem,
        }
    }

    /// The Go expression converting a taken `(code, message, payload)` triple
    /// into an `error` value.
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
/// `error` (unknown codes, marshalling failures), plus the `wvTakeError` slot
/// reader (returning code, message, and a copy of the structured payload
/// buffer), the `wvBrandError` constructor, and the `wvTrap` panic helper
/// non-throwing wrappers check their slot with.
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
    w.line("// code, message, and a copy of its structured payload buffer (nil when");
    w.line("// the code declares no payload fields).");
    w.block(
        "func wvTakeError(cErr *C.weaveffi_error) (int32, string, []byte) {",
        "}",
        |w| {
            w.line("code := int32(cErr.code)");
            w.line("msg := \"\"");
            w.block("if cErr.message != nil {", "}", |w| {
                w.line("msg = C.GoString(cErr.message)");
            });
            w.line("var payload []byte");
            w.block("if cErr.payload_ptr != nil {", "}", |w| {
                w.line(
                    "payload = C.GoBytes(unsafe.Pointer(cErr.payload_ptr), C.int(cErr.payload_len))",
                );
            });
            w.line("C.weaveffi_error_clear(cErr)");
            w.line("return code, msg, payload");
        },
    );
    w.blank();

    w.block(
        "func wvBrandError(code int32, message string, _ []byte) error {",
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
            w.line("code, msg, _ := wvTakeError(cErr)");
            w.line("panic(fmt.Sprintf(\"weaveffi: %s (code %d)\", msg, code))");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Render one declaring module's typed error surface: a
/// `type {TypeName} struct` implementing `error` (so `errors.As` selects on
/// the domain), exported `int32` code constants in the plain-enum const style
/// (`{TypeName}{CodePascal}`), one payload struct per code that declares
/// fields, and the `wvMap{Stem}` helper converting a non-zero slot's
/// `(code, message, payload)` into the typed error (default message when the
/// slot carried none, decoded payload attached when the code declares fields,
/// generic [`ERROR_BRAND`] fallback for unknown codes).
fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding, prefix: &str) {
    let stem = eb.owner_path.to_upper_camel_case();
    let ty = &eb.type_name;
    let dotted = module.segments.join(".");
    let has_payloads = eb.codes.iter().any(|c| !c.fields.is_empty());

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
        if has_payloads {
            w.line("// Payload holds the matched code's structured fields when that code");
            w.line("// declares any (a pointer to the per-code payload struct), else nil.");
            w.line("Payload any");
        }
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

    // One payload struct per code that declares structured fields.
    for c in &eb.codes {
        if c.fields.is_empty() {
            continue;
        }
        let cname = format!("{ty}{}", c.name.to_upper_camel_case());
        let pname = format!("{cname}Payload");
        w.line(format!(
            "// {pname} carries the structured fields of {cname}."
        ));
        w.block(format!("type {pname} struct {{"), "}", |w| {
            for f in &c.fields {
                let fname = f.name.to_upper_camel_case();
                let mut fd = String::new();
                emit_doc(&mut fd, &f.doc, "\t", Some(&fname));
                w.raw(fd);
                w.line(format!("{fname} {}", go_type(&f.ty)));
            }
        });
        w.blank();
    }

    w.line(format!(
        "// wvMap{stem} converts a non-zero code from the `{dotted}` domain into a"
    ));
    w.line(format!(
        "// *{ty}, falling back to the generic *{ERROR_BRAND} for unknown codes."
    ));
    w.block(
        format!("func wvMap{stem}(code int32, message string, payload []byte) error {{"),
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
                if c.fields.is_empty() {
                    w.line(format!("return &{ty}{{Code: code, Message: message}}"));
                } else {
                    let pname = format!("{cname}Payload");
                    w.line(format!("e := &{ty}{{Code: code, Message: message}}"));
                    w.block("if payload != nil {", "}", |w| {
                        w.line("r := &wvReader{buf: payload}");
                        w.line(format!("p := &{pname}{{}}"));
                        for f in &c.fields {
                            let fname = f.name.to_upper_camel_case();
                            emit_buffer_read(
                                w,
                                "r",
                                &format!("p.{fname}"),
                                &f.ty,
                                &fname,
                                0,
                                prefix,
                                &eb.owner_path,
                            );
                        }
                        w.line("r.expectEnd()");
                        w.line("e.Payload = p");
                    });
                    w.line("return e");
                }
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line("return wvBrandError(code, message, payload)");
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

// ── Value-buffer runtime ──

/// The private writer/reader pair implementing the WeaveFFI value-buffer
/// wire format (little-endian, packed, `u32` length prefixes), plus the two
/// buffer copy helpers (`wvCopyBuffer` for owned returns released with
/// `weaveffi_free_bytes`, `wvBorrowBuffer` for borrowed callback/async
/// buffers the producer frees).
///
/// The reader panics on malformed input: a bad buffer is a producer bug (a
/// contract violation), not a recoverable domain error, so it surfaces
/// through the same panic channel a trapped producer error does.
fn render_buffer_runtime(out: &mut String) {
    let mut w = CodeWriter::tabs();
    w.line("// wvWriter serializes values into the WeaveFFI value-buffer format:");
    w.line("// little-endian, packed, u32 length prefixes.");
    w.block("type wvWriter struct {", "}", |w| {
        w.line("buf []byte");
    });
    w.blank();
    w.block("func (w *wvWriter) writeBool(v bool) {", "}", |w| {
        w.line("if v {");
        w.indent();
        w.line("w.buf = append(w.buf, 1)");
        w.dedent();
        w.line("} else {");
        w.indent();
        w.line("w.buf = append(w.buf, 0)");
        w.dedent();
        w.line("}");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI8(v int8) {", "}", |w| {
        w.line("w.buf = append(w.buf, byte(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU8(v uint8) {", "}", |w| {
        w.line("w.buf = append(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI16(v int16) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint16(w.buf, uint16(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU16(v uint16) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint16(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI32(v int32) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint32(w.buf, uint32(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU32(v uint32) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint32(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI64(v int64) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint64(w.buf, uint64(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU64(v uint64) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint64(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeF32(v float32) {", "}", |w| {
        w.line("w.writeU32(math.Float32bits(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeF64(v float64) {", "}", |w| {
        w.line("w.writeU64(math.Float64bits(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeLen(n int) {", "}", |w| {
        w.block("if n < 0 || uint64(n) > uint64(^uint32(0)) {", "}", |w| {
            w.line("panic(\"weaveffi: value-buffer length exceeds u32 range\")");
        });
        w.line("w.writeU32(uint32(n))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeString(v string) {", "}", |w| {
        w.line("w.writeLen(len(v))");
        w.line("w.buf = append(w.buf, v...)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeBytes(v []byte) {", "}", |w| {
        w.line("w.writeLen(len(v))");
        w.line("w.buf = append(w.buf, v...)");
    });
    w.blank();
    w.block(
        "func (w *wvWriter) writeOptionFlag(present bool) {",
        "}",
        |w| {
            w.line("w.writeBool(present)");
        },
    );
    w.blank();

    w.line("// wvReader decodes values from the WeaveFFI value-buffer format. A");
    w.line("// malformed buffer is a producer/consumer contract violation, so every");
    w.line("// read panics (the same channel a trapped producer error uses) instead");
    w.line("// of returning a typed domain error.");
    w.block("type wvReader struct {", "}", |w| {
        w.line("buf []byte");
        w.line("pos int");
    });
    w.blank();
    w.block("func wvMalformed(context string) {", "}", |w| {
        w.line("panic(\"weaveffi: malformed value buffer: \" + context)");
    });
    w.blank();
    w.block(
        "func (r *wvReader) take(n int, context string) []byte {",
        "}",
        |w| {
            w.block("if n < 0 || len(r.buf)-r.pos < n {", "}", |w| {
                w.line("wvMalformed(context)");
            });
            w.line("b := r.buf[r.pos : r.pos+n]");
            w.line("r.pos += n");
            w.line("return b");
        },
    );
    w.blank();
    w.block("func (r *wvReader) readBool() bool {", "}", |w| {
        w.line("switch r.take(1, \"bool\")[0] {");
        w.line("case 0:");
        w.indent();
        w.line("return false");
        w.dedent();
        w.line("case 1:");
        w.indent();
        w.line("return true");
        w.dedent();
        w.line("}");
        w.line("wvMalformed(\"bool byte out of range\")");
        w.line("return false");
    });
    w.blank();
    w.block("func (r *wvReader) readI8() int8 {", "}", |w| {
        w.line("return int8(r.take(1, \"i8\")[0])");
    });
    w.blank();
    w.block("func (r *wvReader) readU8() uint8 {", "}", |w| {
        w.line("return r.take(1, \"u8\")[0]");
    });
    w.blank();
    w.block("func (r *wvReader) readI16() int16 {", "}", |w| {
        w.line("return int16(binary.LittleEndian.Uint16(r.take(2, \"i16\")))");
    });
    w.blank();
    w.block("func (r *wvReader) readU16() uint16 {", "}", |w| {
        w.line("return binary.LittleEndian.Uint16(r.take(2, \"u16\"))");
    });
    w.blank();
    w.block("func (r *wvReader) readI32() int32 {", "}", |w| {
        w.line("return int32(binary.LittleEndian.Uint32(r.take(4, \"i32\")))");
    });
    w.blank();
    w.block("func (r *wvReader) readU32() uint32 {", "}", |w| {
        w.line("return binary.LittleEndian.Uint32(r.take(4, \"u32\"))");
    });
    w.blank();
    w.block("func (r *wvReader) readI64() int64 {", "}", |w| {
        w.line("return int64(binary.LittleEndian.Uint64(r.take(8, \"i64\")))");
    });
    w.blank();
    w.block("func (r *wvReader) readU64() uint64 {", "}", |w| {
        w.line("return binary.LittleEndian.Uint64(r.take(8, \"u64\"))");
    });
    w.blank();
    w.block("func (r *wvReader) readF32() float32 {", "}", |w| {
        w.line("return math.Float32frombits(r.readU32())");
    });
    w.blank();
    w.block("func (r *wvReader) readF64() float64 {", "}", |w| {
        w.line("return math.Float64frombits(r.readU64())");
    });
    w.blank();
    w.block("func (r *wvReader) readLen() int {", "}", |w| {
        w.line("n := int(r.readU32())");
        w.block("if n > len(r.buf)-r.pos {", "}", |w| {
            w.line("wvMalformed(\"length prefix exceeds remaining buffer\")");
        });
        w.line("return n");
    });
    w.blank();
    w.block("func (r *wvReader) readString() string {", "}", |w| {
        w.line("b := r.take(r.readLen(), \"string bytes\")");
        w.block("if !utf8.Valid(b) {", "}", |w| {
            w.line("wvMalformed(\"string is not valid UTF-8\")");
        });
        w.line("return string(b)");
    });
    w.blank();
    w.block("func (r *wvReader) readBytes() []byte {", "}", |w| {
        w.line("b := r.take(r.readLen(), \"byte buffer\")");
        w.line("out := make([]byte, len(b))");
        w.line("copy(out, b)");
        w.line("return out");
    });
    w.blank();
    w.block("func (r *wvReader) readOptionFlag() bool {", "}", |w| {
        w.line("switch r.take(1, \"option flag\")[0] {");
        w.line("case 0:");
        w.indent();
        w.line("return false");
        w.dedent();
        w.line("case 1:");
        w.indent();
        w.line("return true");
        w.dedent();
        w.line("}");
        w.line("wvMalformed(\"option flag byte out of range\")");
        w.line("return false");
    });
    w.blank();
    w.block("func (r *wvReader) expectEnd() {", "}", |w| {
        w.block("if r.pos != len(r.buf) {", "}", |w| {
            w.line("wvMalformed(\"trailing bytes after value\")");
        });
    });
    w.blank();

    w.line("// wvCopyBuffer copies an owned, producer-allocated value buffer into Go");
    w.line("// memory and releases it with weaveffi_free_bytes.");
    w.block(
        "func wvCopyBuffer(ptr *C.uint8_t, length C.size_t) []byte {",
        "}",
        |w| {
            w.block("if ptr == nil {", "}", |w| {
                w.line("return nil");
            });
            w.line("out := C.GoBytes(unsafe.Pointer(ptr), C.int(length))");
            w.line("C.weaveffi_free_bytes(ptr, length)");
            w.line("return out");
        },
    );
    w.blank();
    w.line("// wvBorrowBuffer copies a borrowed value buffer into Go memory. The");
    w.line("// producer keeps ownership and frees it after the borrowing call returns.");
    w.block(
        "func wvBorrowBuffer(ptr *C.uint8_t, length C.size_t) []byte {",
        "}",
        |w| {
            w.block("if ptr == nil {", "}", |w| {
                w.line("return nil");
            });
            w.line("return C.GoBytes(unsafe.Pointer(ptr), C.int(length))");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Value-buffer codegen ──

/// Emit statements appending `expr` (a Go value of type `ty`) to the
/// `wvWriter` named `writer`, following the wire format. `site` and `depth`
/// uniquify the loop locals generated for nested lists and maps.
fn emit_buffer_write(
    w: &mut CodeWriter,
    writer: &str,
    expr: &str,
    ty: &TypeRef,
    site: &str,
    depth: usize,
) {
    match ty {
        TypeRef::Bool => {
            w.line(format!("{writer}.writeBool({expr})"));
        }
        TypeRef::I8 => {
            w.line(format!("{writer}.writeI8({expr})"));
        }
        TypeRef::I16 => {
            w.line(format!("{writer}.writeI16({expr})"));
        }
        TypeRef::I32 => {
            w.line(format!("{writer}.writeI32({expr})"));
        }
        TypeRef::I64 => {
            w.line(format!("{writer}.writeI64({expr})"));
        }
        TypeRef::U8 => {
            w.line(format!("{writer}.writeU8({expr})"));
        }
        TypeRef::U16 => {
            w.line(format!("{writer}.writeU16({expr})"));
        }
        TypeRef::U32 => {
            w.line(format!("{writer}.writeU32({expr})"));
        }
        TypeRef::U64 => {
            w.line(format!("{writer}.writeU64({expr})"));
        }
        TypeRef::F32 => {
            w.line(format!("{writer}.writeF32({expr})"));
        }
        TypeRef::F64 => {
            w.line(format!("{writer}.writeF64({expr})"));
        }
        TypeRef::Handle => {
            w.line(format!("{writer}.writeU64(uint64({expr}))"));
        }
        TypeRef::Enum(_) => {
            w.line(format!("{writer}.writeI32(int32({expr}))"));
        }
        // A typed handle serializes as the u64 value of its opaque pointer.
        TypeRef::TypedHandle(_) => {
            w.line(format!(
                "{writer}.writeU64(uint64(uintptr(unsafe.Pointer({expr}.ptr))))"
            ));
        }
        TypeRef::StringUtf8 => {
            w.line(format!("{writer}.writeString({expr})"));
        }
        TypeRef::Bytes => {
            w.line(format!("{writer}.writeBytes({expr})"));
        }
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            w.line(format!("wvPack{}({writer}, {expr})", go_local(n)));
        }
        TypeRef::Optional(inner) => {
            w.line(format!("if {expr} == nil {{"));
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(false)"));
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(true)"));
            let inner_expr = if optional_derefs(inner) {
                format!("(*{expr})")
            } else {
                expr.to_string()
            };
            emit_buffer_write(w, writer, &inner_expr, inner, site, depth + 1);
            w.dedent();
            w.line("}");
        }
        TypeRef::List(inner) => {
            let e = format!("e{site}{depth}");
            w.line(format!("{writer}.writeLen(len({expr}))"));
            w.block(format!("for _, {e} := range {expr} {{"), "}", |w| {
                emit_buffer_write(w, writer, &e, inner, site, depth + 1);
            });
        }
        TypeRef::Map(k, v) => {
            let kv = format!("k{site}{depth}");
            let vv = format!("v{site}{depth}");
            w.line(format!("{writer}.writeLen(len({expr}))"));
            w.block(format!("for {kv}, {vv} := range {expr} {{"), "}", |w| {
                emit_buffer_write(w, writer, &kv, k, site, depth + 1);
                emit_buffer_write(w, writer, &vv, v, site, depth + 1);
            });
        }
        TypeRef::BorrowedStr | TypeRef::BorrowedBytes => {
            unreachable!("borrowed views are rejected in buffered positions")
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("object references cannot be serialized by value")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Emit statements decoding one value of type `ty` from the `wvReader` named
/// `reader` and assigning it into the pre-declared destination `dst`.
/// `site` and `depth` uniquify the locals generated for nested containers.
#[allow(clippy::too_many_arguments)]
fn emit_buffer_read(
    w: &mut CodeWriter,
    reader: &str,
    dst: &str,
    ty: &TypeRef,
    site: &str,
    depth: usize,
    prefix: &str,
    module: &str,
) {
    match ty {
        TypeRef::Bool => {
            w.line(format!("{dst} = {reader}.readBool()"));
        }
        TypeRef::I8 => {
            w.line(format!("{dst} = {reader}.readI8()"));
        }
        TypeRef::I16 => {
            w.line(format!("{dst} = {reader}.readI16()"));
        }
        TypeRef::I32 => {
            w.line(format!("{dst} = {reader}.readI32()"));
        }
        TypeRef::I64 => {
            w.line(format!("{dst} = {reader}.readI64()"));
        }
        TypeRef::U8 => {
            w.line(format!("{dst} = {reader}.readU8()"));
        }
        TypeRef::U16 => {
            w.line(format!("{dst} = {reader}.readU16()"));
        }
        TypeRef::U32 => {
            w.line(format!("{dst} = {reader}.readU32()"));
        }
        TypeRef::U64 => {
            w.line(format!("{dst} = {reader}.readU64()"));
        }
        TypeRef::F32 => {
            w.line(format!("{dst} = {reader}.readF32()"));
        }
        TypeRef::F64 => {
            w.line(format!("{dst} = {reader}.readF64()"));
        }
        TypeRef::Handle => {
            w.line(format!("{dst} = int64({reader}.readU64())"));
        }
        TypeRef::Enum(n) => {
            w.line(format!("{dst} = {}({reader}.readI32())", go_local(n)));
        }
        TypeRef::TypedHandle(n) => {
            let g = handle_wrapper(n);
            let tag = c_abi_struct_name(n, module, prefix);
            w.line(format!(
                "{dst} = &{g}{{ptr: (*C.{tag})(unsafe.Pointer(uintptr({reader}.readU64())))}}"
            ));
        }
        TypeRef::StringUtf8 => {
            w.line(format!("{dst} = {reader}.readString()"));
        }
        TypeRef::Bytes => {
            w.line(format!("{dst} = {reader}.readBytes()"));
        }
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            w.line(format!("{dst} = wvUnpack{}({reader})", go_local(n)));
        }
        TypeRef::Optional(inner) => {
            let o = format!("o{site}{depth}");
            w.block(format!("if {reader}.readOptionFlag() {{"), "}", |w| {
                w.line(format!("var {o} {}", go_type(inner)));
                emit_buffer_read(w, reader, &o, inner, site, depth + 1, prefix, module);
                if optional_derefs(inner) {
                    w.line(format!("{dst} = &{o}"));
                } else {
                    w.line(format!("{dst} = {o}"));
                }
            });
        }
        TypeRef::List(inner) => {
            let n = format!("n{site}{depth}");
            let i = format!("i{site}{depth}");
            w.line(format!("{n} := {reader}.readLen()"));
            w.line(format!("{dst} = make([]{}, {n})", go_type(inner)));
            w.block(format!("for {i} := range {dst} {{"), "}", |w| {
                emit_buffer_read(
                    w,
                    reader,
                    &format!("{dst}[{i}]"),
                    inner,
                    site,
                    depth + 1,
                    prefix,
                    module,
                );
            });
        }
        TypeRef::Map(k, v) => {
            let n = format!("n{site}{depth}");
            let i = format!("i{site}{depth}");
            let kv = format!("k{site}{depth}");
            let vv = format!("v{site}{depth}");
            let gk = go_type(k);
            let gv = go_type(v);
            w.line(format!("{n} := {reader}.readLen()"));
            w.line(format!("{dst} = make(map[{gk}]{gv}, {n})"));
            w.block(format!("for {i} := 0; {i} < {n}; {i}++ {{"), "}", |w| {
                w.line(format!("var {kv} {gk}"));
                emit_buffer_read(w, reader, &kv, k, site, depth + 1, prefix, module);
                w.line(format!("var {vv} {gv}"));
                emit_buffer_read(w, reader, &vv, v, site, depth + 1, prefix, module);
                w.line(format!("{dst}[{kv}] = {vv}"));
            });
        }
        TypeRef::BorrowedStr | TypeRef::BorrowedBytes => {
            unreachable!("borrowed views are rejected in buffered positions")
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("object references cannot be serialized by value")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

// ── Typed handles ──

/// Collect every typed-handle referent reachable from the model's type
/// positions, deduplicated by wrapper name in first-occurrence order (which
/// keeps the emitted set deterministic).
fn collect_typed_handles(model: &BindingModel, prefix: &str) -> Vec<(String, String)> {
    fn visit(
        ty: &TypeRef,
        module: &str,
        prefix: &str,
        seen: &mut HashSet<String>,
        out: &mut Vec<(String, String)>,
    ) {
        match ty {
            TypeRef::TypedHandle(n) => {
                let name = handle_wrapper(n);
                if seen.insert(name.clone()) {
                    out.push((name, c_abi_struct_name(n, module, prefix)));
                }
            }
            TypeRef::Optional(i) | TypeRef::List(i) | TypeRef::Iterator(i) => {
                visit(i, module, prefix, seen, out);
            }
            TypeRef::Map(k, v) => {
                visit(k, module, prefix, seen, out);
                visit(v, module, prefix, seen, out);
            }
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in &model.modules {
        for s in &m.structs {
            for f in &s.fields {
                visit(&f.ty, &m.path, prefix, &mut seen, &mut out);
            }
        }
        for e in &m.enums {
            for v in &e.variants {
                for f in &v.fields {
                    visit(&f.ty, &m.path, prefix, &mut seen, &mut out);
                }
            }
        }
        if let Some(eb) = &m.error {
            if eb.declared_here {
                for c in &eb.codes {
                    for f in &c.fields {
                        visit(&f.ty, &m.path, prefix, &mut seen, &mut out);
                    }
                }
            }
        }
        for cb in &m.callbacks {
            for p in &cb.params {
                visit(&p.ty, &m.path, prefix, &mut seen, &mut out);
            }
        }
        for f in m.callables() {
            for p in &f.params {
                visit(&p.ty, &m.path, prefix, &mut seen, &mut out);
            }
            if let Some(ret) = &f.ret {
                visit(ret, &m.path, prefix, &mut seen, &mut out);
            }
        }
    }
    out
}

/// Render one wrapper struct per typed-handle referent. A typed handle is a
/// borrowed opaque id with no destroy symbol, so the wrapper carries no
/// `Close`.
fn render_typed_handles(out: &mut String, handles: &[(String, String)]) {
    let mut w = CodeWriter::tabs();
    for (name, tag) in handles {
        w.line(format!(
            "// {name} is a typed handle naming a producer-owned resource. It wraps"
        ));
        w.line("// the opaque C pointer and owes no release call.");
        w.block(format!("type {name} struct {{"), "}", |w| {
            w.line(format!("ptr *C.{tag}"));
        });
        w.blank();
    }
    out.push_str(&w.finish());
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

    if imports.fmt || imports.iter || imports.unsafe_ptr || imports.sync || imports.buffer_runtime {
        out.push_str("\nimport (\n");
        if imports.buffer_runtime {
            out.push_str("\t\"encoding/binary\"\n");
        }
        if imports.fmt {
            out.push_str("\t\"fmt\"\n");
        }
        if imports.iter {
            out.push_str("\t\"iter\"\n");
        }
        if imports.buffer_runtime {
            out.push_str("\t\"math\"\n");
        }
        if imports.sync {
            out.push_str("\t\"sync\"\n");
        }
        if imports.buffer_runtime {
            out.push_str("\t\"unicode/utf8\"\n");
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

    if imports.buffer_runtime {
        render_buffer_runtime(&mut out);
    }

    if has_async || has_listeners {
        render_callback_registry(&mut out, has_listeners);
    }

    let handles = collect_typed_handles(model, prefix);
    if !handles.is_empty() {
        render_typed_handles(&mut out, &handles);
    }

    for m in &model.modules {
        let stem = domain_stem(m);
        if let Some(eb) = &m.error {
            // Emit the typed domain once, in its declaring module; inheriting
            // submodules reference the ancestor's type through `wvMap{Stem}`.
            if eb.declared_here {
                render_error(&mut out, m, eb, prefix);
            }
        }
        for e in &m.enums {
            // A plain C-style enum becomes an `int32` + constants; a rich
            // (algebraic) enum becomes a sealed sum type. Each renderer skips
            // the other kind.
            render_enum(&mut out, e);
            render_rich_enum(&mut out, prefix, &m.path, e);
        }
        for s in &m.structs {
            render_struct(&mut out, prefix, &m.path, s);
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
            let err = ErrCtx::of(f, stem.as_deref());
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
///
/// Every callback argument is borrowed for the dispatch: buffered values are
/// decoded from the borrowed `(ptr, len)` pair, strings and bytes are copied,
/// and object pointers are wrapped without adopting ownership.
fn emit_cb_param_arg(
    out: &mut String,
    idx: usize,
    p: &ParamBinding,
    prefix: &str,
    module: &str,
) -> String {
    let arg = format!("arg{idx}");
    let mut w = CodeWriter::tabs().with_depth(1);
    if is_buffered(&p.ty) {
        let ptr_slot = &p.abi[0].name;
        let len_slot = &p.abi[1].name;
        w.line(format!(
            "rArg{idx} := &wvReader{{buf: wvBorrowBuffer({ptr_slot}, {len_slot})}}"
        ));
        w.line(format!("var {arg} {}", go_type(&p.ty)));
        emit_buffer_read(
            &mut w,
            &format!("rArg{idx}"),
            &arg,
            &p.ty,
            &format!("Arg{idx}"),
            0,
            prefix,
            module,
        );
        w.line(format!("rArg{idx}.expectEnd()"));
        out.push_str(&w.finish());
        return arg;
    }
    let n = &p.abi[0].name;
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
        | TypeRef::F64
        | TypeRef::Enum(_) => {
            w.line(format!("{arg} := {}", go_scalar_conv(n, &p.ty)));
        }
        TypeRef::Bool => {
            w.line(format!("{arg} := cToBool({n})"));
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
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
            w.line(format!("{arg} := {}", go_wrap_expr(&p.ty, n)));
        }
        // Only an optional interface reaches here unbuffered: a nullable
        // borrowed object pointer.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("every other optional is buffered")
            };
            let g = go_local(name);
            w.line(format!("var {arg} *{g}"));
            w.block(format!("if {n} != nil {{"), "}", |w| {
                w.line(format!("{arg} = &{g}{{ptr: {n}}}"));
            });
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
///
/// Result buffers (strings, bytes, value buffers) are borrowed for the
/// callback's duration per the shared async protocol: they are decoded or
/// deep copied here and never freed (the producer releases them after the
/// callback returns). Owned interface results are the exception: the callback
/// receives ownership and the wrapper adopts the pointer (its `Close` calls
/// the destroy symbol).
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
    if is_buffered(ty) {
        // Borrowed for the callback's duration: decode, do not free.
        w.line("rRes := &wvReader{buf: wvBorrowBuffer(result_ptr, result_len)}");
        w.line(format!("var val {}", go_type(ty)));
        emit_buffer_read(&mut w, "rRes", "val", ty, "Res", 0, prefix, module);
        w.line("rRes.expectEnd()");
        w.line(format!("ch <- {outcome}{{val: val}}"));
        out.push_str(&w.finish());
        return;
    }
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
        | TypeRef::F64
        | TypeRef::Enum(_) => {
            w.line(format!(
                "ch <- {outcome}{{val: {}}}",
                go_scalar_conv("result", ty)
            ));
        }
        TypeRef::Bool => {
            w.line(format!("ch <- {outcome}{{val: cToBool(result)}}"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            // Borrowed for the callback's duration: copy, do not free.
            w.line("val := \"\"");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoString(result)");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            // Borrowed for the callback's duration: copy, do not free.
            w.line("var val []byte");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoBytes(unsafe.Pointer(result), C.int(result_len))");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        // An owned interface result is adopted by the wrapper (its Close
        // calls the destroy symbol); a typed handle is a borrowed id.
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
            w.line(format!(
                "ch <- {outcome}{{val: {}}}",
                go_wrap_expr(ty, "result")
            ));
        }
        // Only an optional interface reaches here unbuffered.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("every other optional is buffered")
            };
            let g = go_local(name);
            w.line(format!("var val *{g}"));
            w.block("if result != nil {", "}", |w| {
                w.line(format!("val = &{g}{{ptr: result}}"));
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("async iterator returns are rejected upstream"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// An async callable: a blocking Go wrapper that launches the C call with a
/// completion trampoline and waits on a buffered channel, plus the outcome
/// type and the exported trampoline itself.
///
/// The error split follows the shared plan's [`ErrorStrategy`]. A throwing
/// wrapper returns `(T, error)` and the trampoline maps a reported error
/// through the domain (`wvMap{Stem}`). A plain wrapper returns bare `T`; a
/// reported error can only be a producer bug, so the trampoline wraps it as
/// the generic brand error (never the typed domain) and the wrapper panics
/// with it on the calling goroutine (the trampoline itself must never panic:
/// it runs on a producer thread entered from C). With `receiver` set, the
/// wrapper is a method on that wrapper type passing `s.ptr` as the leading
/// launch argument.
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
    // A non-throwing function's error slot can only carry a producer bug:
    // brand it generically rather than dressing it as a typed domain error.
    let map_err = if err.throws {
        err.map_call("wvTakeError(err)")
    } else {
        "wvBrandError(wvTakeError(err))".to_string()
    };
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
    w.line("// Blocks the calling goroutine until the async producer completes.");
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
    // Rich (algebraic) enums are value sum types rendered by
    // `render_rich_enum`; only plain C-style enums are int32s.
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

/// Render a rich (algebraic) enum as an idiomatic Go sum type: a sealed
/// interface (`type Shape interface { isShape() }`) with one struct per
/// variant (`ShapeCircle`, holding that variant's fields as exported struct
/// fields), plus the pack/unpack pair serializing the `i32` tag followed by
/// the active variant's fields in wire order. Rich enums have no C symbols;
/// values only cross the ABI inside value buffers.
///
/// A plain C-style enum is skipped here (it is handled by [`render_enum`]).
fn render_rich_enum(out: &mut String, prefix: &str, module: &str, e: &EnumBinding) {
    if !e.is_rich() {
        return;
    }
    let name = e.name.to_upper_camel_case();

    let mut w = CodeWriter::tabs();
    if e.doc.is_some() {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "", Some(&name));
        w.raw(d);
        w.line("//");
    }
    w.line(format!(
        "// {name} is a sealed sum type: exactly one of its variant structs is the"
    ));
    w.line("// value at a time.");
    w.block(format!("type {name} interface {{"), "}", |w| {
        w.line(format!("is{name}()"));
    });
    w.blank();

    for v in &e.variants {
        let vn = format!("{name}{}", v.name.to_upper_camel_case());
        let mut vd = String::new();
        emit_doc(&mut vd, &v.doc, "", Some(&vn));
        if vd.is_empty() {
            w.line(format!("// {vn} is the `{}` variant of {name}.", v.name));
        } else {
            w.raw(vd);
        }
        if v.fields.is_empty() {
            w.line(format!("type {vn} struct{{}}"));
        } else {
            w.block(format!("type {vn} struct {{"), "}", |w| {
                for f in &v.fields {
                    let fname = f.name.to_upper_camel_case();
                    let mut fd = String::new();
                    emit_doc(&mut fd, &f.doc, "\t", Some(&fname));
                    w.raw(fd);
                    w.line(format!("{fname} {}", go_type(&f.ty)));
                }
            });
        }
        w.blank();
        w.line(format!("func ({vn}) is{name}() {{}}"));
        w.blank();
    }

    w.line(format!(
        "// wvPack{name} appends v to w in the value-buffer wire format."
    ));
    w.block(
        format!("func wvPack{name}(w *wvWriter, v {name}) {{"),
        "}",
        |w| {
            w.line("switch x := v.(type) {");
            for v in &e.variants {
                let vn = format!("{name}{}", v.name.to_upper_camel_case());
                w.line(format!("case {vn}:"));
                w.indent();
                w.line(format!("w.writeI32({})", v.value));
                for f in &v.fields {
                    let fname = f.name.to_upper_camel_case();
                    let site = format!("{}{fname}", v.name.to_upper_camel_case());
                    emit_buffer_write(w, "w", &format!("x.{fname}"), &f.ty, &site, 0);
                }
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "panic(\"weaveffi: {name} value is not one of its variants\")"
            ));
            w.dedent();
            w.line("}");
        },
    );
    w.blank();

    w.line(format!("// wvUnpack{name} decodes one {name} from r."));
    w.block(
        format!("func wvUnpack{name}(r *wvReader) {name} {{"),
        "}",
        |w| {
            w.line("switch r.readI32() {");
            for v in &e.variants {
                let vn = format!("{name}{}", v.name.to_upper_camel_case());
                w.line(format!("case {}:", v.value));
                w.indent();
                if v.fields.is_empty() {
                    w.line(format!("return {vn}{{}}"));
                } else {
                    w.line(format!("var x {vn}"));
                    for f in &v.fields {
                        let fname = f.name.to_upper_camel_case();
                        let site = format!("{}{fname}", v.name.to_upper_camel_case());
                        emit_buffer_read(
                            w,
                            "r",
                            &format!("x.{fname}"),
                            &f.ty,
                            &site,
                            0,
                            prefix,
                            module,
                        );
                    }
                    w.line("return x");
                }
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "panic(\"weaveffi: malformed value buffer: {name} tag out of range\")"
            ));
            w.dedent();
            w.line("}");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Structs ──

/// Render one record as a plain Go value struct with exported, typed fields,
/// plus its pack/unpack pair serializing the fields in declaration (wire)
/// order. Records have no C symbols: no create, no destroy, no getters, no
/// builders; instances only cross the ABI inside value buffers.
fn render_struct(out: &mut String, prefix: &str, module: &str, s: &StructBinding) {
    let name = s.name.to_upper_camel_case();

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        for f in &s.fields {
            let fname = f.name.to_upper_camel_case();
            let mut fd = String::new();
            emit_doc(&mut fd, &f.doc, "\t", Some(&fname));
            w.raw(fd);
            w.line(format!("{fname} {}", go_type(&f.ty)));
        }
    });
    w.blank();

    w.line(format!(
        "// wvPack{name} appends v to w in the value-buffer wire format."
    ));
    w.block(
        format!("func wvPack{name}(w *wvWriter, v {name}) {{"),
        "}",
        |w| {
            for f in &s.fields {
                let fname = f.name.to_upper_camel_case();
                emit_buffer_write(w, "w", &format!("v.{fname}"), &f.ty, &fname, 0);
            }
        },
    );
    w.blank();

    w.line(format!("// wvUnpack{name} decodes one {name} from r."));
    w.block(
        format!("func wvUnpack{name}(r *wvReader) {name} {{"),
        "}",
        |w| {
            w.line(format!("var v {name}"));
            for f in &s.fields {
                let fname = f.name.to_upper_camel_case();
                emit_buffer_read(
                    w,
                    "r",
                    &format!("v.{fname}"),
                    &f.ty,
                    &fname,
                    0,
                    prefix,
                    module,
                );
            }
            w.line("return v");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Interfaces ──

/// Render one interface as an opaque-object wrapper: a struct owning the
/// `*C.{c_tag}` handle, freed by an explicit `Close` (idempotent, nils the
/// pointer).
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
        let err = ErrCtx::of(c, stem);
        render_function(out, prefix, &m.path, c, &go_name, None, err);
    }

    for f in &iface.methods {
        let go_name = f.name.to_upper_camel_case();
        let err = ErrCtx::of(f, stem);
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, &m.path, f, ab, &go_name, Some(&name), err);
        } else {
            render_function(out, prefix, &m.path, f, &go_name, Some(&name), err);
        }
    }

    for f in &iface.statics {
        let go_name = format!("{name}{}", f.name.to_upper_camel_case());
        let err = ErrCtx::of(f, stem);
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
/// result out. An iterator-returning callable renders through
/// [`render_iterator_fn`] as a lazy sequence instead. With `receiver` set,
/// the wrapper is a method on that wrapper type passing `s.ptr` as the
/// leading C argument.
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
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator_fn(out, prefix, module, f, ib, go_name, receiver, err);
        return;
    }

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

    if let Some(ref ret) = f.ret {
        emit_return_out_params(&mut pre, &mut c_args, ret);
    }

    pre.push_str("\tvar cErr C.weaveffi_error\n");
    c_args.push("&cErr".into());

    let args = c_args.join(", ");

    w.block(header, "}", |w| {
        w.raw(pre.as_str());

        if f.ret.is_some() {
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
/// Buffered and bytes elements arrive as a `const uint8_t*` buffer pointer.
fn iter_out_item_type(inner: &TypeRef, prefix: &str, module: &str) -> String {
    if is_buffered(inner) {
        return "*C.uint8_t".into();
    }
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "*C.char".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "*C.uint8_t".into(),
        TypeRef::TypedHandle(n) | TypeRef::Interface(n) => {
            format!("*C.{}", c_abi_struct_name(n, module, prefix))
        }
        _ => c_scalar_type(inner, prefix, module).unwrap_or_else(|| "C.int64_t".into()),
    }
}

/// Re-indent `block` by one tab per non-empty line, used to move depth-1
/// staging code inside the sequence closure body.
fn indent_block(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    for line in block.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push('\t');
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Emit the statements converting one freshly-pulled `next` slot (`outItem`,
/// plus `outLen` for bytes/buffered elements) into a Go value bound to
/// `item`, releasing the slot per the protocol's [`ElemFree`] plan: strings
/// are freed after copying, bytes and buffered elements are copied/decoded
/// and released with `weaveffi_free_bytes` (via `wvCopyBuffer`), and by-value
/// elements owe nothing.
fn emit_iter_elem_bind(
    w: &mut CodeWriter,
    inner: &TypeRef,
    ef: &ElemFree,
    prefix: &str,
    module: &str,
) {
    match ef {
        ElemFree::String => {
            w.line("item := C.GoString(outItem)");
            w.line("C.weaveffi_free_string(outItem)");
        }
        ElemFree::Bytes => {
            if matches!(inner, TypeRef::Bytes | TypeRef::BorrowedBytes) {
                w.line("item := wvCopyBuffer(outItem, outLen)");
            } else {
                w.line("rItem := &wvReader{buf: wvCopyBuffer(outItem, outLen)}");
                w.line(format!("var item {}", go_type(inner)));
                emit_buffer_read(w, "rItem", "item", inner, "Item", 0, prefix, module);
                w.line("rItem.expectEnd()");
            }
        }
        ElemFree::None => match inner {
            TypeRef::Bool => {
                w.line("item := cToBool(outItem)");
            }
            // Typed handles and interfaces are opaque pointers the consumer
            // adopts, even though `elem_free` owes no runtime call for them.
            TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
                w.line(format!("item := {}", go_wrap_expr(inner, "outItem")));
            }
            _ => {
                let conv = go_scalar_conv("outItem", inner);
                w.line(format!("item := {conv}"));
            }
        },
    }
}

/// An `iter<T>`-returning callable, rendered per the shared
/// [`weaveffi_core::plan::IteratorProtocol`] pull contract as Go's standard
/// lazy iteration idiom (the `iter` package, Go 1.23+):
///
/// - A non-throwing function returns `iter.Seq[T]`. A launch or per-`next`
///   error can only be a producer bug, so it panics with the weaveffi
///   message via `wvTrap` ([`ErrorStrategy::Trap`]).
/// - A throwing function returns `iter.Seq2[T, error]`. A launch or
///   per-`next` domain error is yielded as the final `(zero, err)` pair and
///   iteration stops ([`ErrorStrategy::Throws`]).
///
/// The producer iterator is launched lazily inside the returned closure, so
/// an unused sequence allocates nothing on the producer side. One C `next`
/// call runs per consumer step, each yielded element is released per the
/// protocol's [`ElemFree`] plan after conversion, and the destroy runs
/// exactly once through a `defer` inside the closure, whether the sequence
/// is exhausted, stops on an error, or is abandoned by an early `break`.
#[allow(clippy::too_many_arguments)]
fn render_iterator_fn(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    ib: &IteratorBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let proto = ib.protocol(f);
    let throws = matches!(proto.error, ErrorStrategy::Throws);
    let elem = &ib.elem;
    let elem_go = go_type(elem);
    let item_ty = iter_out_item_type(elem, prefix, module);
    let has_len = matches!(proto.elem_free, ElemFree::Bytes);
    let zero = go_zero(elem);

    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();
    let (seq_ty, yield_ty) = if throws {
        (
            format!("iter.Seq2[{elem_go}, error]"),
            format!("func({elem_go}, error) bool"),
        )
    } else {
        (
            format!("iter.Seq[{elem_go}]"),
            format!("func({elem_go}) bool"),
        )
    };
    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}) {seq_ty} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}) {seq_ty} {{", go_params.join(", ")),
    };

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    w.raw(doc);
    w.line("// Returns a lazy sequence: the producer iterator is launched on first");
    w.line("// iteration and one producer next call runs per element. The iterator is");
    w.line("// destroyed exactly once, whether the sequence is exhausted or abandoned");
    w.line("// early; each range over the sequence launches a fresh producer iterator.");
    if throws {
        w.line("// A launch or per-element error is yielded as the final (zero value,");
        w.line("// error) pair, and iteration stops.");
    } else {
        w.line("// A reported error can only be a producer bug and panics with the");
        w.line("// weaveffi message.");
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    // Parameter staging runs inside the closure so C strings and buffers are
    // live at launch time and each range restages them.
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
    c_args.push("&cErr".into());

    // Statements surfacing a non-zero error slot: yield the mapped domain
    // error and stop when throwing, trap when plain.
    let emit_err_check = |w: &mut CodeWriter, slot: &str| {
        if throws {
            let map = err.map_call(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                w.line(format!("yield({zero}, {map})"));
                w.line("return");
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    };

    let next_args = if has_len {
        "it, &outItem, &outLen, &iterErr"
    } else {
        "it, &outItem, &iterErr"
    };

    w.block(header, "}", |w| {
        w.block(format!("return func(yield {yield_ty}) {{"), "}", |w| {
            w.raw(indent_block(&pre));
            w.line("var cErr C.weaveffi_error");
            w.line(format!(
                "it := C.{}({})",
                ib.launch.symbol,
                c_args.join(", ")
            ));
            emit_err_check(w, "cErr");
            w.line(format!("defer C.{}(it)", ib.destroy_symbol));
            w.block("for {", "}", |w| {
                w.line(format!("var outItem {item_ty}"));
                if has_len {
                    w.line("var outLen C.size_t");
                }
                w.line("var iterErr C.weaveffi_error");
                w.line(format!("ok := C.{}({next_args}) != 0", ib.next.symbol));
                emit_err_check(w, "iterErr");
                w.block("if !ok {", "}", |w| {
                    w.line("return");
                });
                emit_iter_elem_bind(w, elem, &proto.elem_free, prefix, module);
                let yield_call = if throws {
                    "if !yield(item, nil) {"
                } else {
                    "if !yield(item) {"
                };
                w.block(yield_call, "}", |w| {
                    w.line("return");
                });
            });
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

// ── Parameter conversion ──

/// Emit the staging statements and C argument expressions for one Go
/// parameter. A buffered parameter is packed into a `wvWriter` and passed as
/// a borrowed `(ptr, len)` pair; the C-owned encoding lives in Go memory kept
/// alive for the duration of the call by cgo's argument-pinning rules.
fn emit_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    ty: &TypeRef,
    prefix: &str,
    module: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    if is_buffered(ty) {
        let n = name.to_upper_camel_case();
        w.line(format!("w{n} := &wvWriter{{}}"));
        emit_buffer_write(&mut w, &format!("w{n}"), name, ty, &n, 0);
        w.line(format!("var c{n}Ptr *C.uint8_t"));
        w.block(format!("if len(w{n}.buf) > 0 {{"), "}", |w| {
            w.line(format!(
                "c{n}Ptr = (*C.uint8_t)(unsafe.Pointer(&w{n}.buf[0]))"
            ));
        });
        args.push(format!("c{n}Ptr"));
        args.push(format!("C.size_t(len(w{n}.buf))"));
        pre.push_str(&w.finish());
        return;
    }
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
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => args.push(format!("{name}.ptr")),

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

        // Only an optional interface reaches here unbuffered: a nullable
        // borrowed object pointer, null meaning none.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(n) = inner.as_ref() else {
                unreachable!("every other optional is buffered")
            };
            let ct = c_abi_struct_name(n, module, prefix);
            let cv = format!("c{}", name.to_upper_camel_case());
            w.line(format!("var {cv} *C.{ct}"));
            w.block(format!("if {name} != nil {{"), "}", |w| {
                w.line(format!("{cv} = {name}.ptr"));
            });
            args.push(cv);
        }

        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    pre.push_str(&w.finish());
}

// ── Return out-params ──

/// Emit the out-parameter locals a return type needs. Bytes and buffered
/// returns carry one trailing `size_t* out_len` slot; everything else has
/// none.
fn emit_return_out_params(pre: &mut String, args: &mut Vec<String>, ty: &TypeRef) {
    if is_buffered(ty) || matches!(ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
        let mut w = CodeWriter::tabs().with_depth(1);
        w.line("var cOutLen C.size_t");
        args.push("&cOutLen".into());
        pre.push_str(&w.finish());
    }
}

// ── Return conversion ──

/// Emit the success-path return conversion. `tail` is [`ErrCtx::ok_tail`]:
/// `", nil"` when the wrapper also returns an error, empty when plain.
///
/// A buffered return is copied out of the producer-allocated buffer (which
/// `wvCopyBuffer` releases with `weaveffi_free_bytes`), decoded, and checked
/// for trailing bytes.
fn emit_return(out: &mut String, ty: &TypeRef, prefix: &str, module: &str, tail: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    if is_buffered(ty) {
        w.line("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}");
        w.line(format!("var goResult {}", go_type(ty)));
        emit_buffer_read(&mut w, "rRes", "goResult", ty, "Res", 0, prefix, module);
        w.line("rRes.expectEnd()");
        w.line(format!("return goResult{tail}"));
        out.push_str(&w.finish());
        return;
    }
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
        | TypeRef::F64
        | TypeRef::Enum(_) => {
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
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
            w.line(format!("return {}{tail}", go_wrap_expr(ty, "result")));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.block("if result == nil {", "}", |w| {
                w.line(format!("return nil{tail}"));
            });
            w.line("goResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))");
            w.line("C.weaveffi_free_bytes(result, cOutLen)");
            w.line(format!("return goResult{tail}"));
        }
        // Only an optional interface reaches here unbuffered: a nullable
        // owned object pointer.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(n) = inner.as_ref() else {
                unreachable!("every other optional is buffered")
            };
            let g = go_local(n);
            w.block("if result == nil {", "}", |w| {
                w.line(format!("return nil{tail}"));
            });
            w.line(format!("return &{g}{{ptr: result}}{tail}"));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => {
            unreachable!("iterator returns render through the lazy sequence path")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
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
            version: "0.6.0".into(),
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
            fields: vec![],
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
    /// `Entry` record, the eviction listener, and the nested `kv.stats`
    /// submodule taking a cross-module interface parameter.
    fn kv_api() -> Api {
        let mut stats = module("stats");
        stats.structs = vec![StructDef {
            name: "Stats".into(),
            doc: None,
            fields: vec![field("total_entries", TypeRef::I64)],
        }];
        stats.functions = vec![throwing(func_of(
            "get_stats",
            // Cross-module references reach generators pre-qualified by the
            // validator's resolve step; mirror that spelling here.
            vec![param("store", TypeRef::Interface("kv.Store".into()))],
            Some(TypeRef::Record("Stats".into())),
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
                variant("Volatile", 0, vec![]),
                variant("Persistent", 1, vec![]),
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
                    Some(TypeRef::Optional(Box::new(TypeRef::Record("Entry".into())))),
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
                variant("Personal", 0, vec![]),
                variant("Work", 1, vec![]),
                variant("Other", 2, vec![]),
            ],
        }];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
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
                    Some(TypeRef::Record("Contact".into())),
                )),
                throwing(func_of(
                    "get",
                    vec![param("id", TypeRef::I64)],
                    Some(TypeRef::Record("Contact".into())),
                )),
                func_of(
                    "list",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
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

    /// A module with one rich (algebraic) enum used across params and
    /// returns.
    fn shapes_api() -> Api {
        let mut m = module("shapes");
        m.enums = vec![EnumDef {
            name: "Shape".into(),
            doc: None,
            variants: vec![
                variant("Empty", 0, vec![]),
                variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
                variant(
                    "Labeled",
                    3,
                    vec![
                        field("label", TypeRef::StringUtf8),
                        field("count", TypeRef::U8),
                    ],
                ),
            ],
        }];
        m.functions = vec![
            func_of(
                "describe",
                vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                Some(TypeRef::StringUtf8),
            ),
            func_of(
                "scale",
                vec![
                    param("shape", TypeRef::RichEnum("Shape".into())),
                    param("factor", TypeRef::F64),
                ],
                Some(TypeRef::RichEnum("Shape".into())),
            ),
        ];
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
            variants: vec![variant("Red", 0, vec![])],
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

    // ── Buffered params and returns ──

    #[test]
    fn struct_return_decodes_buffer() {
        let mut m = module("contacts");
        m.functions = vec![func_of(
            "get_contact",
            vec![param("id", TypeRef::Handle)],
            Some(TypeRef::Record("Contact".into())),
        )];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func GetContact(id int64) Contact {"),
            "record return should be a bare value struct: {go}"
        );
        assert!(
            go.contains("var cOutLen C.size_t"),
            "missing out_len slot: {go}"
        );
        assert!(
            go.contains("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}"),
            "buffered return must copy then free through wvCopyBuffer: {go}"
        );
        assert!(
            go.contains("goResult = wvUnpackContact(rRes)"),
            "missing record decode: {go}"
        );
        assert!(
            go.contains("rRes.expectEnd()"),
            "decoder must reject trailing bytes: {go}"
        );
        assert!(
            !go.contains("&Contact{ptr:"),
            "records no longer wrap C pointers: {go}"
        );
    }

    #[test]
    fn buffered_record_param_packs() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func_of(
            "save_contact",
            vec![param("contact", TypeRef::Record("Contact".into()))],
            None,
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func SaveContact(contact Contact) {"),
            "record param should be a bare value struct: {go}"
        );
        assert!(
            go.contains("wContact := &wvWriter{}"),
            "missing writer staging: {go}"
        );
        assert!(
            go.contains("wvPackContact(wContact, contact)"),
            "missing record pack call: {go}"
        );
        assert!(
            go.contains("cContactPtr = (*C.uint8_t)(unsafe.Pointer(&wContact.buf[0]))"),
            "missing buffer pointer staging: {go}"
        );
        assert!(
            go.contains(
                "C.weaveffi_contacts_save_contact(cContactPtr, C.size_t(len(wContact.buf)), &cErr)"
            ),
            "buffered param must pass ptr + len: {go}"
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
            go.contains("if query == nil {"),
            "missing nil check for optional: {go}"
        );
        assert!(
            go.contains("wQuery.writeOptionFlag(false)"),
            "missing absent flag write: {go}"
        );
        assert!(
            go.contains("wQuery.writeString((*query))"),
            "missing dereferenced string write: {go}"
        );
        assert!(
            go.contains("C.size_t(len(wQuery.buf))"),
            "optional param must pass the encoded length: {go}"
        );
    }

    #[test]
    fn optional_struct_return() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func_of(
            "find",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Find(id int32) *Contact {"),
            "optional struct return: {go}"
        );
        assert!(
            go.contains("if rRes.readOptionFlag() {"),
            "missing option flag check: {go}"
        );
        assert!(
            go.contains("oRes0 = wvUnpackContact(rRes)"),
            "missing inner decode: {go}"
        );
        assert!(
            go.contains("goResult = &oRes0"),
            "present value must be pointer-wrapped: {go}"
        );
    }

    #[test]
    fn list_return_decodes_buffer() {
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
        assert!(
            go.contains("nRes0 := rRes.readLen()"),
            "missing count read: {go}"
        );
        assert!(
            go.contains("goResult = make([]int32, nRes0)"),
            "missing slice allocation: {go}"
        );
        assert!(
            go.contains("goResult[iRes0] = rRes.readI32()"),
            "missing element decode: {go}"
        );
    }

    #[test]
    fn struct_list_return_decodes_elements() {
        let mut m = module("contacts");
        m.functions = vec![func_of(
            "list_contacts",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
        )];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func ListContacts() []Contact {"),
            "record lists hold values, not pointers: {go}"
        );
        assert!(
            go.contains("goResult[iRes0] = wvUnpackContact(rRes)"),
            "missing per-element record decode: {go}"
        );
    }

    #[test]
    fn optional_i32_param_and_return() {
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
            go.contains("wId.writeI32((*id))"),
            "missing dereferenced scalar write: {go}"
        );
        assert!(
            go.contains("var goResult *int32"),
            "optional i32 return should be *int32: {go}"
        );
        assert!(
            go.contains("oRes0 = rRes.readI32()"),
            "missing scalar decode: {go}"
        );
    }

    #[test]
    fn map_return_decodes_buffer() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "counts",
            vec![],
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Counts() map[string]int32 {"),
            "missing map return sig: {go}"
        );
        assert!(
            go.contains("goResult = make(map[string]int32, nRes0)"),
            "missing map allocation: {go}"
        );
        assert!(
            go.contains("kRes0 = rRes.readString()"),
            "missing key decode: {go}"
        );
        assert!(
            go.contains("vRes0 = rRes.readI32()"),
            "missing value decode: {go}"
        );
        assert!(
            go.contains("goResult[kRes0] = vRes0"),
            "missing map insert: {go}"
        );
    }

    #[test]
    fn map_param_packs() {
        let mut m = module("metrics");
        m.functions = vec![func_of(
            "record_counts",
            vec![param(
                "counts",
                TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            )],
            None,
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func RecordCounts(counts map[string]int32) {"),
            "missing map param sig: {go}"
        );
        assert!(
            go.contains("wCounts.writeLen(len(counts))"),
            "missing count write: {go}"
        );
        assert!(
            go.contains("for kCounts0, vCounts0 := range counts {"),
            "missing pair loop: {go}"
        );
        assert!(
            go.contains("wCounts.writeString(kCounts0)"),
            "missing key write: {go}"
        );
        assert!(
            go.contains("wCounts.writeI32(vCounts0)"),
            "missing value write: {go}"
        );
    }

    #[test]
    fn optional_scalar_return_decodes_buffer() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "capacity",
            vec![],
            Some(TypeRef::Optional(Box::new(TypeRef::I64))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Capacity() *int64 {"),
            "optional scalar return should be a pointer: {go}"
        );
        assert!(
            go.contains("wvCopyBuffer(result, cOutLen)"),
            "buffered return must copy then free: {go}"
        );
        assert!(
            go.contains("oRes0 = rRes.readI64()"),
            "missing scalar decode: {go}"
        );
        assert!(go.contains("\t\"unsafe\"\n"), "unsafe import needed: {go}");
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
            go.contains("func wvMapStore(code int32, message string, payload []byte) error {"),
            "missing domain mapping helper: {go}"
        );
        assert!(
            go.contains("message = \"save failed\""),
            "missing default message fill: {go}"
        );
        assert!(
            go.contains("return wvBrandError(code, message, payload)"),
            "unknown codes must fall back to the brand error: {go}"
        );
        assert!(
            go.contains(&format!("type {ERROR_BRAND} struct {{")),
            "missing generic brand error: {go}"
        );
    }

    #[test]
    fn wv_take_error_returns_payload() {
        let go = rg(&store_api());
        assert!(
            go.contains("func wvTakeError(cErr *C.weaveffi_error) (int32, string, []byte) {"),
            "wvTakeError must return the payload triple: {go}"
        );
        assert!(
            go.contains(
                "payload = C.GoBytes(unsafe.Pointer(cErr.payload_ptr), C.int(cErr.payload_len))"
            ),
            "wvTakeError must copy the payload before clearing: {go}"
        );
        assert!(
            go.contains("code, msg, _ := wvTakeError(cErr)"),
            "wvTrap discards the payload: {go}"
        );
    }

    #[test]
    fn error_payload_fields_decode() {
        let mut m = module("store");
        m.errors = Some(ErrorDomain {
            name: "StoreError".into(),
            codes: vec![
                code("SaveFailed", 1, "save failed"),
                ErrorCode {
                    name: "Conflict".into(),
                    code: 2,
                    message: "write conflict".into(),
                    doc: None,
                    fields: vec![
                        field("key", TypeRef::StringUtf8),
                        field("attempts", TypeRef::I32),
                    ],
                },
            ],
        });
        m.functions = vec![throwing(func_of(
            "save",
            vec![param("data", TypeRef::StringUtf8)],
            None,
        ))];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("Payload any"),
            "domain with payload codes must expose Payload: {go}"
        );
        assert!(
            go.contains("type StoreErrorConflictPayload struct {"),
            "missing per-code payload struct: {go}"
        );
        assert!(
            go.contains("Key string") && go.contains("Attempts int32"),
            "payload struct must carry the declared fields: {go}"
        );
        assert!(
            go.contains("p.Key = r.readString()") && go.contains("p.Attempts = r.readI32()"),
            "payload fields must decode in wire order: {go}"
        );
        assert!(
            go.contains("e.Payload = p"),
            "decoded payload must attach to the error: {go}"
        );
        assert!(
            go.contains("r.expectEnd()"),
            "payload decode must reject trailing bytes: {go}"
        );
        // A code without fields keeps the simple construction.
        assert!(
            go.contains("return &StoreError{Code: code, Message: message}"),
            "codes without fields skip payload plumbing: {go}"
        );
    }

    // ── Enums, records, rich enums ──

    #[test]
    fn enum_generation() {
        let mut m = module("paint");
        m.enums = vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![
                variant("Red", 0, vec![]),
                variant("Green", 1, vec![]),
                variant("Blue", 2, vec![]),
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
    fn record_is_plain_value_struct() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
            ],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(go.contains("type Contact struct {"), "missing struct: {go}");
        assert!(
            go.contains("\tName string\n"),
            "missing typed Name field: {go}"
        );
        assert!(
            go.contains("\tAge int32\n"),
            "missing typed Age field: {go}"
        );
        assert!(
            go.contains("func wvPackContact(w *wvWriter, v Contact) {"),
            "missing pack function: {go}"
        );
        assert!(
            go.contains("w.writeString(v.Name)") && go.contains("w.writeI32(v.Age)"),
            "pack must serialize fields in order: {go}"
        );
        assert!(
            go.contains("func wvUnpackContact(r *wvReader) Contact {"),
            "missing unpack function: {go}"
        );
        assert!(
            go.contains("v.Name = r.readString()") && go.contains("v.Age = r.readI32()"),
            "unpack must decode fields in order: {go}"
        );
        // Records have no C symbols: no handle wrapping, no destroy, no
        // getters, no builders.
        assert!(
            !go.contains("ptr *C.weaveffi_contacts_Contact"),
            "records must not wrap a C pointer: {go}"
        );
        assert!(
            !go.contains("Contact_destroy") && !go.contains("func (s *Contact)"),
            "records have no destroy or getters: {go}"
        );
        assert!(
            !go.contains("ContactBuilder"),
            "records have no builders: {go}"
        );
    }

    #[test]
    fn record_optional_string_field() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field(
                "email",
                TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
            )],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("\tEmail *string\n"),
            "optional string field should be *string: {go}"
        );
        assert!(
            go.contains("if v.Email == nil {"),
            "pack must branch on presence: {go}"
        );
        assert!(
            go.contains("w.writeString((*v.Email))"),
            "pack must dereference the present value: {go}"
        );
        assert!(
            go.contains("oEmail0 = r.readString()") && go.contains("v.Email = &oEmail0"),
            "unpack must pointer-wrap the present value: {go}"
        );
    }

    #[test]
    fn record_bytes_field_roundtrips() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("photo", TypeRef::Bytes),
            ],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("w.writeBytes(v.Photo)"),
            "bytes fields pack as length-prefixed buffers: {go}"
        );
        assert!(
            go.contains("v.Photo = r.readBytes()"),
            "bytes fields decode as copies: {go}"
        );
    }

    #[test]
    fn record_enum_field() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("contact_type", TypeRef::Enum("ContactType".into()))],
        }];
        m.enums = vec![EnumDef {
            name: "ContactType".into(),
            doc: None,
            variants: vec![variant("Personal", 0, vec![])],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("\tContactType ContactType\n"),
            "missing enum-typed field: {go}"
        );
        assert!(
            go.contains("w.writeI32(int32(v.ContactType))"),
            "enum fields pack as i32: {go}"
        );
        assert!(
            go.contains("v.ContactType = ContactType(r.readI32())"),
            "enum fields decode through the enum type: {go}"
        );
    }

    #[test]
    fn rich_enum_is_sealed_sum_type() {
        let go = rg(&shapes_api());
        assert!(
            go.contains("type Shape interface {"),
            "missing sealed interface: {go}"
        );
        assert!(go.contains("\tisShape()\n"), "missing sealing method: {go}");
        assert!(
            go.contains("type ShapeEmpty struct{}"),
            "unit variant should be an empty struct: {go}"
        );
        assert!(
            go.contains("type ShapeCircle struct {") && go.contains("\tRadius float64\n"),
            "data variant carries typed fields: {go}"
        );
        assert!(
            go.contains("type ShapeLabeled struct {")
                && go.contains("\tLabel string\n")
                && go.contains("\tCount uint8\n"),
            "multi-field variant carries all fields: {go}"
        );
        assert!(
            go.contains("func (ShapeEmpty) isShape() {}")
                && go.contains("func (ShapeCircle) isShape() {}")
                && go.contains("func (ShapeLabeled) isShape() {}"),
            "every variant implements the sealing method: {go}"
        );
        // Rich enums have no C symbols.
        assert!(
            !go.contains("Shape_destroy")
                && !go.contains("NewShapeCircle")
                && !go.contains("Tag()"),
            "rich enums have no constructors, tag readers, or destroy: {go}"
        );
    }

    #[test]
    fn rich_enum_pack_unpack() {
        let go = rg(&shapes_api());
        assert!(
            go.contains("func wvPackShape(w *wvWriter, v Shape) {"),
            "missing pack function: {go}"
        );
        assert!(
            go.contains("switch x := v.(type) {"),
            "pack switches on the variant type: {go}"
        );
        assert!(
            go.contains("case ShapeCircle:")
                && go.contains("w.writeI32(1)")
                && go.contains("w.writeF64(x.Radius)"),
            "pack writes the tag then the variant fields: {go}"
        );
        assert!(
            go.contains("case ShapeLabeled:")
                && go.contains("w.writeI32(3)")
                && go.contains("w.writeString(x.Label)")
                && go.contains("w.writeU8(x.Count)"),
            "non-contiguous tags use the declared values: {go}"
        );
        assert!(
            go.contains("panic(\"weaveffi: Shape value is not one of its variants\")"),
            "pack rejects foreign implementations: {go}"
        );
        assert!(
            go.contains("func wvUnpackShape(r *wvReader) Shape {"),
            "missing unpack function: {go}"
        );
        assert!(
            go.contains("return ShapeEmpty{}"),
            "unit variants decode to the empty struct: {go}"
        );
        assert!(
            go.contains("x.Radius = r.readF64()"),
            "variant fields decode in order: {go}"
        );
        assert!(
            go.contains("panic(\"weaveffi: malformed value buffer: Shape tag out of range\")"),
            "unpack rejects unknown tags: {go}"
        );
        // The rich enum crosses the ABI as a buffer in both directions.
        assert!(
            go.contains("func Describe(shape Shape) string {"),
            "rich enum param is the bare interface type: {go}"
        );
        assert!(
            go.contains("wvPackShape(wShape, shape)"),
            "rich enum param packs through the writer: {go}"
        );
        assert!(
            go.contains("func Scale(shape Shape, factor float64) Shape {"),
            "rich enum return is the bare interface type: {go}"
        );
        assert!(
            go.contains("goResult = wvUnpackShape(rRes)"),
            "rich enum return decodes from the buffer: {go}"
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

    // ── The value-buffer runtime ──

    #[test]
    fn buffer_runtime_emitted_once() {
        let mut a = module("alpha");
        a.structs = vec![StructDef {
            name: "A".into(),
            doc: None,
            fields: vec![field("x", TypeRef::I32)],
        }];
        let mut b = module("beta");
        b.structs = vec![StructDef {
            name: "B".into(),
            doc: None,
            fields: vec![field("y", TypeRef::F32)],
        }];
        let go = rg(&api_of(vec![a, b]));
        assert_eq!(
            go.matches("type wvWriter struct {").count(),
            1,
            "runtime must be emitted exactly once: {go}"
        );
        assert_eq!(
            go.matches("type wvReader struct {").count(),
            1,
            "runtime must be emitted exactly once: {go}"
        );
        assert!(
            go.contains("binary.LittleEndian"),
            "wire format is little-endian: {go}"
        );
        assert!(
            go.contains("if !utf8.Valid(b) {"),
            "string decode must validate UTF-8: {go}"
        );
        assert!(
            go.contains("wvMalformed(\"length prefix exceeds remaining buffer\")"),
            "reader must reject oversized length prefixes: {go}"
        );
        assert!(
            go.contains("wvMalformed(\"trailing bytes after value\")"),
            "reader must reject trailing bytes: {go}"
        );
        assert!(
            go.contains("C.weaveffi_free_bytes(ptr, length)"),
            "wvCopyBuffer must free the producer buffer: {go}"
        );
        assert!(
            go.contains("\t\"encoding/binary\"\n")
                && go.contains("\t\"math\"\n")
                && go.contains("\t\"unicode/utf8\"\n"),
            "runtime imports must be present: {go}"
        );
    }

    #[test]
    fn no_buffer_runtime_when_unneeded() {
        let go = rg(&calculator_api());
        assert!(
            !go.contains("wvWriter"),
            "scalar-only surfaces need no buffer runtime: {go}"
        );
        assert!(
            !go.contains("\"encoding/binary\""),
            "scalar-only surfaces must not import binary: {go}"
        );
    }

    // ── Typed handles ──

    #[test]
    fn typed_handle_wrapper_and_flow() {
        let mut m = module("vault");
        m.structs = vec![StructDef {
            name: "Session".into(),
            doc: None,
            fields: vec![field("token", TypeRef::TypedHandle("Token".into()))],
        }];
        m.functions = vec![
            func_of("open", vec![], Some(TypeRef::TypedHandle("Token".into()))),
            func_of(
                "revoke",
                vec![param("t", TypeRef::TypedHandle("Token".into()))],
                None,
            ),
        ];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("type TokenHandle struct {"),
            "missing handle wrapper: {go}"
        );
        assert!(
            go.contains("ptr *C.weaveffi_vault_Token"),
            "wrapper must hold the opaque C pointer: {go}"
        );
        assert!(
            go.contains("func Open() *TokenHandle {"),
            "handle return should be the wrapper pointer: {go}"
        );
        assert!(
            go.contains("return &TokenHandle{ptr: result}"),
            "missing handle wrap on return: {go}"
        );
        assert!(
            go.contains("C.weaveffi_vault_revoke(t.ptr, &cErr)"),
            "handle params pass the wrapped pointer: {go}"
        );
        // No destroy: a typed handle is a borrowed id.
        assert!(
            !go.contains("func (s *TokenHandle) Close()"),
            "typed handles owe no release call: {go}"
        );
        // Inside buffers the handle serializes as the pointer's u64 value.
        assert!(
            go.contains("w.writeU64(uint64(uintptr(unsafe.Pointer(v.Token.ptr))))"),
            "handle fields pack as u64: {go}"
        );
        assert!(
            go.contains(
                "v.Token = &TokenHandle{ptr: (*C.weaveffi_vault_Token)(unsafe.Pointer(uintptr(r.readU64())))}"
            ),
            "handle fields decode back into the wrapper: {go}"
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
            go.contains("ch <- wvOutcomeIoRead{err: wvBrandError(wvTakeError(err))}"),
            "plain async trampoline brands the error, never the domain: {go}"
        );
        assert!(
            go.contains("return outcome.val"),
            "plain async wrapper returns the outcome value: {go}"
        );
        // The completion callback borrows its result buffers: copy, no free.
        assert!(
            !go.contains("C.weaveffi_free_string(result)"),
            "borrowed async result buffers must not be freed: {go}"
        );
        assert!(
            go.contains("val = C.GoString(result)"),
            "async string results must be copied before the callback returns: {go}"
        );
        assert!(
            go.contains("// Blocks the calling goroutine until the async producer completes."),
            "async wrapper must document that it blocks: {go}"
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

    #[test]
    fn async_record_result_decodes_borrowed_buffer() {
        let mut m = module("metrics");
        m.structs = vec![StructDef {
            name: "Stats".into(),
            doc: None,
            fields: vec![field("total", TypeRef::I64)],
        }];
        m.functions = vec![{
            let mut f = func_of("load", vec![], Some(TypeRef::Record("Stats".into())));
            f.r#async = true;
            f
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains(
                "extern void goWv_weaveffi_metrics_load_callback(void* context, weaveffi_error* err, uint8_t* result_ptr, size_t result_len);"
            ),
            "buffered async callback carries borrowed ptr + len slots: {go}"
        );
        assert!(
            go.contains("rRes := &wvReader{buf: wvBorrowBuffer(result_ptr, result_len)}"),
            "async result buffer is borrowed, never freed: {go}"
        );
        assert!(
            go.contains("val = wvUnpackStats(rRes)"),
            "async record result decodes inside the trampoline: {go}"
        );
        assert!(
            !go.contains("wvCopyBuffer(result_ptr"),
            "the producer frees the async result buffer, not the consumer: {go}"
        );
        assert!(
            go.contains("func Load() Stats {"),
            "async record wrapper returns the value struct: {go}"
        );
    }

    // ── Listeners and callbacks ──

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

    #[test]
    fn callback_buffered_param_decodes_borrowed_buffer() {
        let mut m = module("feed");
        m.callbacks = vec![CallbackDef {
            name: "OnBatch".into(),
            doc: None,
            params: vec![param("items", TypeRef::List(Box::new(TypeRef::StringUtf8)))],
        }];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains(
                "extern void goWv_weaveffi_feed_OnBatch_fn(uint8_t* items_ptr, size_t items_len, void* context);"
            ),
            "buffered callback param carries ptr + len slots: {go}"
        );
        assert!(
            go.contains("rArg0 := &wvReader{buf: wvBorrowBuffer(items_ptr, items_len)}"),
            "callback buffers are borrowed, never freed: {go}"
        );
        assert!(
            go.contains("arg0 = make([]string, nArg00)"),
            "list argument decodes before dispatch: {go}"
        );
        assert!(
            go.contains("cb(arg0)"),
            "decoded value is handed to the user callback: {go}"
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
        // Throwing method: `(T, error)` with the receiver's ptr leading. The
        // optional scalar parameter is buffered now.
        assert!(
            go.contains(
                "func (s *Store) Put(key string, value []byte, kind EntryKind, ttlSeconds *int64) (bool, error) {"
            ),
            "missing throwing method: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_kv_Store_put(s.ptr, cKey, cValuePtr, cValueLen, C.weaveffi_kv_EntryKind(kind), cTtlSecondsPtr, C.size_t(len(wTtlSeconds.buf)), &cErr)"),
            "method must pass s.ptr and the buffered optional's ptr + len: {go}"
        );
        assert!(
            go.contains("wTtlSeconds.writeI64((*ttlSeconds))"),
            "optional scalar param packs into the writer: {go}"
        );
        assert!(
            go.contains("return false, wvMapKv(wvTakeError(&cErr))"),
            "throwing bool method returns its zero value with the error: {go}"
        );
        // Optional record return through a method decodes from the buffer.
        assert!(
            go.contains("func (s *Store) Get(key string) (*Entry, error) {"),
            "missing optional-return method: {go}"
        );
        assert!(
            go.contains("oRes0 = wvUnpackEntry(rRes)"),
            "optional record return decodes the present value: {go}"
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
    fn optional_interface_param_stays_pointer() {
        let mut m = module("kv");
        m.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![],
            methods: vec![],
            statics: vec![],
        }];
        m.functions = vec![func_of(
            "inspect",
            vec![param(
                "store",
                TypeRef::Optional(Box::new(TypeRef::Interface("Store".into()))),
            )],
            None,
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func Inspect(store *Store) {"),
            "optional interface param stays a nullable wrapper pointer: {go}"
        );
        assert!(
            go.contains("var cStore *C.weaveffi_kv_Store"),
            "missing nullable C pointer staging: {go}"
        );
        assert!(
            go.contains("cStore = store.ptr"),
            "present value passes the wrapped pointer: {go}"
        );
        assert!(
            !go.contains("wStore"),
            "optional interfaces are never buffered: {go}"
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
        // A throwing iterator returns iter.Seq2[T, error]; the standard iter
        // package is imported.
        assert!(
            go.contains("func (s *Store) ListKeys(prefix *string) iter.Seq2[string, error] {"),
            "throwing iterator method returns iter.Seq2[T, error]: {go}"
        );
        assert!(go.contains("\t\"iter\"\n"), "iter import needed: {go}");
        // The launch runs lazily inside the returned closure (first pull),
        // never in the wrapper body itself. The optional string param is
        // buffered and staged inside the closure.
        let fn_start = go
            .find("func (s *Store) ListKeys(")
            .expect("ListKeys wrapper");
        let fn_text = &go[fn_start..];
        let closure = fn_text
            .find("return func(yield func(string, error) bool) {")
            .expect("sequence closure in ListKeys");
        let launch = fn_text
            .find(
                "it := C.weaveffi_kv_Store_list_keys(s.ptr, cPrefixPtr, C.size_t(len(wPrefix.buf)), &cErr)",
            )
            .expect("launch in ListKeys");
        assert!(
            closure < launch,
            "launch must run inside the closure: {fn_text}"
        );
        // Launch errors are yielded as the final (zero, err) pair.
        assert!(
            go.contains("yield(\"\", wvMapKv(wvTakeError(&cErr)))"),
            "launch errors are yielded through the domain: {go}"
        );
        // Destroy is deferred inside the closure so an early break still
        // destroys exactly once.
        assert!(
            go.contains("defer C.weaveffi_kv_Store_ListKeysIterator_destroy(it)"),
            "iterator destroy must be deferred inside the closure: {go}"
        );
        // One producer next call per consumer step.
        assert!(
            go.contains(
                "ok := C.weaveffi_kv_Store_ListKeysIterator_next(it, &outItem, &iterErr) != 0"
            ),
            "iterator must pull one element per step: {go}"
        );
        assert!(
            go.contains("yield(\"\", wvMapKv(wvTakeError(&iterErr)))"),
            "per-element errors are yielded through the domain: {go}"
        );
        // Each yielded string element is freed after copying.
        assert!(
            go.contains("item := C.GoString(outItem)\n\t\t\tC.weaveffi_free_string(outItem)"),
            "string elements must be freed after copying: {go}"
        );
        assert!(
            go.contains("if !yield(item, nil) {"),
            "elements are yielded with a nil error: {go}"
        );
        // No hidden drain into a slice.
        assert!(
            !fn_text[..fn_text.find("\n}\n").unwrap()].contains("append("),
            "iterator must not drain into a slice: {fn_text}"
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
            go.contains("func GetMessages() iter.Seq[string] {"),
            "plain iterator returns iter.Seq[T]: {go}"
        );
        assert!(
            go.contains("return func(yield func(string) bool) {"),
            "plain iterator returns a single-value sequence closure: {go}"
        );
        assert!(
            go.contains("wvTrap(&cErr)"),
            "plain iterator traps launch errors: {go}"
        );
        assert!(
            go.contains("wvTrap(&iterErr)"),
            "plain iterator traps per-element errors: {go}"
        );
        assert!(
            go.contains("defer C.weaveffi_events_GetMessagesIterator_destroy(it)"),
            "plain iterator defers destroy inside the closure: {go}"
        );
        assert!(
            go.contains("if !yield(item) {"),
            "plain iterator yields bare elements: {go}"
        );
        assert!(
            !go.contains("func GetMessages() []string"),
            "plain iterator must not drain into a slice: {go}"
        );
    }

    #[test]
    fn iterator_buffered_elements_decode_and_free() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func_of(
            "iter_contacts",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("func IterContacts() iter.Seq[Contact] {"),
            "record iterator yields value structs: {go}"
        );
        assert!(
            go.contains("var outItem *C.uint8_t") && go.contains("var outLen C.size_t"),
            "buffered elements arrive as ptr + len slots: {go}"
        );
        assert!(
            go.contains("(it, &outItem, &outLen, &iterErr) != 0"),
            "next must pass the element length slot: {go}"
        );
        assert!(
            go.contains("rItem := &wvReader{buf: wvCopyBuffer(outItem, outLen)}"),
            "each element must be copied and freed through wvCopyBuffer: {go}"
        );
        assert!(
            go.contains("item = wvUnpackContact(rItem)"),
            "each element decodes through the record unpack: {go}"
        );
        assert!(
            go.contains("rItem.expectEnd()"),
            "element decode must reject trailing bytes: {go}"
        );
    }

    #[test]
    fn cross_module_interface_param_borrows() {
        let go = rg(&kv_api());
        assert!(
            go.contains("func GetStats(store *Store) (Stats, error) {"),
            "nested-module function takes the wrapper, returns the record value: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_kv_stats_get_stats(store.ptr, &cOutLen, &cErr)"),
            "interface params borrow the wrapped pointer: {go}"
        );
        assert!(
            go.contains("return Stats{}, wvMapKv(wvTakeError(&cErr))"),
            "inheriting submodule maps through the ancestor domain, zeroing the record: {go}"
        );
        assert!(
            go.contains("goResult = wvUnpackStats(rRes)"),
            "cross-module record return decodes from the buffer: {go}"
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
            go.contains("func wvMapKv(code int32, message string, payload []byte) error {"),
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
        assert!(go.contains("\tFirstName string\n"), "{go}");
        assert!(go.contains("\tEmail *string\n"), "{go}");
        assert!(go.contains("type ContactBook struct {"), "{go}");
        assert!(go.contains("ptr *C.weaveffi_contacts_ContactBook"), "{go}");
        assert!(
            go.contains("func (s *ContactBook) Add(firstName string, lastName string, email *string, contactType ContactType) (Contact, error) {"),
            "{go}"
        );
        assert!(
            go.contains("func (s *ContactBook) Get(id int64) (Contact, error) {"),
            "{go}"
        );
        assert!(
            go.contains("func (s *ContactBook) List() []Contact {"),
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
            go.contains("func wvMapContacts(code int32, message string, payload []byte) error {"),
            "{go}"
        );
        assert!(
            go.contains("return Contact{}, wvMapContacts(wvTakeError(&cErr))"),
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
        assert!(
            go_mod.contains("go 1.23"),
            "go.mod must require Go 1.23 for the iter package: {go_mod}"
        );

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
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func_of(
            "find_contact",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::Record("Contact".into())),
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
        let decode = fn_text
            .find("wvCopyBuffer(result, cOutLen)")
            .expect("buffered decode in FindContact");
        assert!(
            err_check < decode,
            "error must be checked before decoding the return buffer: {fn_text}"
        );
    }

    #[test]
    fn go_flag_check_on_optional_return() {
        let mut m = module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func_of(
            "find_contact",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
        )];
        let go = rg(&api_of(vec![m]));

        let fn_start = go.find("func FindContact(").expect("FindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        let flag_check = fn_text
            .find("if rRes.readOptionFlag() {")
            .expect("flag check in FindContact");
        let decode = fn_text
            .find("wvUnpackContact(rRes)")
            .expect("Contact decode in FindContact");
        assert!(
            flag_check < decode,
            "optional record return must check the flag before decoding: {fn_text}"
        );
    }

    #[test]
    fn string_list_return_decodes_from_buffer() {
        let mut m = module("store");
        m.functions = vec![func_of(
            "list_keys",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
        )];
        let go = rg(&api_of(vec![m]));
        assert!(
            go.contains("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}"),
            "list return decodes from one owned buffer: {go}"
        );
        assert!(
            go.contains("goResult[iRes0] = rRes.readString()"),
            "string elements decode in place: {go}"
        );
        assert!(
            !go.contains("unsafe.Slice("),
            "parallel-array decoding is gone: {go}"
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
