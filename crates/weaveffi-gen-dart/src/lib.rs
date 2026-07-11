//! Dart (`dart:ffi`) binding generator for WeaveFFI.
//!
//! Emits a Dart package (`pubspec.yaml` + library) with `dart:ffi`
//! bindings over the C ABI for use in Flutter and Dart projects.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToSnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FieldBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
    RichVariantBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::{ElemFree, ErrorStrategy};
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`DartGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DartConfig {
    /// Dart package name (recorded in `pubspec.yaml`). Defaults to
    /// `"weaveffi"`.
    pub package_name: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// function and listener names, so a `contacts` module exports
    /// `createContact` rather than `contactsCreateContact`. Set to `false`
    /// to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the `dart:ffi` bindings call the
    /// same exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for DartConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl DartConfig {
    /// Returns the configured Dart package name, falling back to `"weaveffi"`.
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

/// Dart backend: emits a Dart package (`pubspec.yaml` plus library) with
/// `dart:ffi` bindings over the C ABI.
pub struct DartGenerator;

impl LanguageBackend for DartGenerator {
    type Config = DartConfig;

    fn name(&self) -> &'static str {
        "dart"
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
        let dart_dir = out_dir.join("dart");
        let lib_dir = dart_dir.join("lib");
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                lib_dir.join("weaveffi.dart"),
                render_dart_module(api, model, config),
            ),
            OutputFile::new(
                dart_dir.join("pubspec.yaml"),
                render_pubspec(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
            OutputFile::new(
                dart_dir.join("README.md"),
                render_readme(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
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
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        // The lib base in the generated source follows the same rule the
        // module uses (`pkg::resolve(api, None, basename)`), so reconstruct it
        // identically to swap the loader.
        let lib_base = pkg::resolve(api, None, Some(input_basename)).ident_name();
        let lib = &ctx.binaries.lib_name;

        let module_src = render_dart_module(api, model, config)
            .replace(&dart_loader_original(&lib_base), &dart_loader_packaged(lib));

        let dart_dir = out_dir.join("dart");
        let mut files = vec![
            PackagedFile::text(dart_dir.join("lib").join("weaveffi.dart"), module_src),
            PackagedFile::text(
                dart_dir.join("pubspec.yaml"),
                render_pubspec(&package, input_basename),
            ),
            PackagedFile::text(
                dart_dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];
        // Bundle every prebuilt library under native/<platform-id>/.
        for nb in &ctx.binaries.binaries {
            let dest = dart_dir
                .join("native")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(DartGenerator);

/// Reproduce the exact `_openLibrary` block `render_dart_module` emits in
/// `generate` mode for `lib_base`, so the packager can swap it.
fn dart_loader_original(lib_base: &str) -> String {
    let mut out = String::new();
    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str("  // An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a\n");
    out.push_str("  // specific build artifact regardless of its file name or location.\n");
    out.push_str("  final override = Platform.environment['WEAVEFFI_LIBRARY'];\n");
    out.push_str(
        "  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);\n",
    );
    out.push_str(&format!(
        "  if (Platform.isMacOS) return DynamicLibrary.open('lib{lib_base}.dylib');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isLinux) return DynamicLibrary.open('lib{lib_base}.so');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isWindows) return DynamicLibrary.open('{lib_base}.dll');\n"
    ));
    out.push_str(
        "  throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');\n",
    );
    out.push_str("}\n");
    out
}

/// The packaged `_openLibrary` for `lib`: try the bundled `native/<platform>/`
/// libraries (relative to the working directory) before the bare system name.
/// `WEAVEFFI_LIBRARY` still overrides.
fn dart_loader_packaged(lib: &str) -> String {
    let mut out = String::new();
    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str("  final override = Platform.environment['WEAVEFFI_LIBRARY'];\n");
    out.push_str(
        "  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);\n",
    );
    out.push_str("  final candidates = <String>[];\n");
    out.push_str("  if (Platform.isMacOS) {\n");
    out.push_str(&format!(
        "    candidates.addAll(['native/darwin-arm64/lib{lib}.dylib', 'native/darwin-x64/lib{lib}.dylib', 'lib{lib}.dylib']);\n"
    ));
    out.push_str("  } else if (Platform.isWindows) {\n");
    out.push_str(&format!(
        "    candidates.addAll(['native/windows-x64/{lib}.dll', '{lib}.dll']);\n"
    ));
    out.push_str("  } else {\n");
    out.push_str(&format!(
        "    candidates.addAll(['native/linux-x64/lib{lib}.so', 'native/linux-arm64/lib{lib}.so', 'lib{lib}.so']);\n"
    ));
    out.push_str("  }\n");
    out.push_str("  for (final candidate in candidates) {\n");
    out.push_str("    try {\n");
    out.push_str("      return DynamicLibrary.open(candidate);\n");
    out.push_str("    } catch (_) {}\n");
    out.push_str("  }\n");
    out.push_str(
        "  throw UnsupportedError('Could not load the native library for ${Platform.operatingSystem}');\n",
    );
    out.push_str("}\n");
    out
}

/// README for a packaged Dart artifact that bundles native libraries.
fn render_packaged_readme(
    package: &ResolvedPackage,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = package.name.clone();
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `native/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {name} (Dart)

Auto-generated `dart:ffi` bindings with prebuilt native libraries bundled under
`native/<platform>/`. The loader prefers a bundled library (resolved relative to
the working directory) and falls back to the system search path;
`WEAVEFFI_LIBRARY` overrides both.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}

fn dart_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle => "int".into(),
        TypeRef::F32 | TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "List<int>".into(),
        // Records, rich enums, C-style enums, typed handles, and interfaces
        // all surface as bare local Dart classes. A cross-module reference
        // (resolved to e.g. `kv.Store`) must still name the local `Store`
        // class, not the qualified IR name.
        TypeRef::TypedHandle(n)
        | TypeRef::Enum(n)
        | TypeRef::Record(n)
        | TypeRef::RichEnum(n)
        | TypeRef::Interface(n) => local_type_name(n).to_upper_camel_case(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Optional(inner) => format!("{}?", dart_type(inner)),
        TypeRef::List(inner) => format!("List<{}>", dart_type(inner)),
        TypeRef::Iterator(inner) => format!("Iterable<{}>", dart_type(inner)),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", dart_type(k), dart_type(v)),
    }
}

fn dart_nullable_type_for_builder_field(ty: &TypeRef) -> String {
    let t = dart_type(ty);
    if t.ends_with('?') {
        t
    } else {
        format!("{t}?")
    }
}

fn native_ffi_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "Int8".into(),
        TypeRef::I16 => "Int16".into(),
        TypeRef::I32 => "Int32".into(),
        TypeRef::U8 => "Uint8".into(),
        TypeRef::U16 => "Uint16".into(),
        TypeRef::U32 => "Uint32".into(),
        TypeRef::U64 => "Uint64".into(),
        TypeRef::I64 | TypeRef::Handle => "Int64".into(),
        TypeRef::F32 => "Float".into(),
        TypeRef::F64 => "Double".into(),
        // A C `bool` is one byte; `Bool` keeps by-value slots, boxed
        // optionals, and element strides in step with the producer.
        TypeRef::Bool => "Bool".into(),
        TypeRef::Enum(_) => "Int32".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Pointer<Uint8>".into(),
        TypeRef::TypedHandle(_)
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::Interface(_) => "Pointer<Void>".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Optional(inner) => native_ffi_type(inner),
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => "Pointer<Void>".into(),
    }
}

fn dart_ffi_type(ty: &TypeRef) -> String {
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
        | TypeRef::Enum(_) => "int".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::F32 | TypeRef::F64 => "double".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Pointer<Uint8>".into(),
        TypeRef::TypedHandle(_)
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::Interface(_) => "Pointer<Void>".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Optional(inner) => dart_ffi_type(inner),
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => "Pointer<Void>".into(),
    }
}

// ── Complex-type marshaling (inputs, getters, returns) ──

/// dart:ffi (native, dart) types of a leaf scalar passed by value or stored
/// as a boxed or array element. `Bool` is one byte, matching the producer's C
/// `bool`, so element strides and boxed-scalar frees stay honest.
fn scalar_ffi(ty: &TypeRef) -> (&'static str, &'static str) {
    match ty {
        TypeRef::I8 => ("Int8", "int"),
        TypeRef::I16 => ("Int16", "int"),
        TypeRef::U8 => ("Uint8", "int"),
        TypeRef::U16 => ("Uint16", "int"),
        TypeRef::U32 => ("Uint32", "int"),
        TypeRef::U64 => ("Uint64", "int"),
        TypeRef::I32 | TypeRef::Enum(_) => ("Int32", "int"),
        TypeRef::Bool => ("Bool", "bool"),
        TypeRef::I64 | TypeRef::Handle => ("Int64", "int"),
        TypeRef::F32 => ("Float", "double"),
        TypeRef::F64 => ("Double", "double"),
        _ => ("Int64", "int"),
    }
}

/// dart:ffi pointer type of the array staged for a `[T]`/map-side *input* (the C
/// element is `const char*` for strings, `T*` for handles, or a value scalar).
fn input_array_ffi(elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Pointer<Utf8>>".into(),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => "Pointer<Pointer<Void>>".into(),
        _ => format!("Pointer<{}>", scalar_ffi(elem).0),
    }
}

/// The (native, dart) FFI typedef slot pairs a single input parameter expands
/// into. Simple types stay one slot (matching [`native_ffi_type`]); bytes/list/
/// map fan out to the ABI's `(ptr, len)` / `(keys, vals, len)` shape; nullable
/// scalars pass through a pointer.
fn input_slots(ty: &TypeRef) -> Vec<(String, String)> {
    let ptr = |s: &str| (s.to_string(), s.to_string());
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![ptr("Pointer<Uint8>"), ("Size".into(), "int".into())]
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            vec![ptr(&input_array_ffi(inner)), ("Size".into(), "int".into())]
        }
        TypeRef::Map(k, v) => vec![
            ptr(&input_array_ffi(k)),
            ptr(&input_array_ffi(v)),
            ("Size".into(), "int".into()),
        ],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![ptr("Pointer<Utf8>")],
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => vec![ptr("Pointer<Void>")],
            other => vec![ptr(&format!("Pointer<{}>", scalar_ffi(other).0))],
        },
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![ptr("Pointer<Utf8>")],
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
            vec![ptr("Pointer<Void>")]
        }
        _ => {
            let (n, d) = scalar_ffi(ty);
            vec![(n.into(), d.into())]
        }
    }
}

/// Emit pre-call staging for one input (`name`), returning the call-argument
/// expressions it contributes (in ABI order) and appending any cleanup
/// statements to `frees`. Mirrors the `(ptr, len)` / `(keys, vals, len)` ABI.
fn emit_input(out: &mut String, name: &str, ty: &TypeRef, frees: &mut Vec<String>) -> Vec<String> {
    match ty {
        TypeRef::Bool => vec![name.to_string()],
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::TypedHandle(_) | TypeRef::Record(_) | TypeRef::RichEnum(_)
        | TypeRef::Interface(_) => {
            vec![format!("{name}._handle")]
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
        | TypeRef::Handle => {
            vec![name.to_string()]
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let p = format!("{name}Ptr");
            let mut w = CodeWriter::two_space().with_depth(1);
            w.line(format!("final {p} = {name}.toNativeUtf8();"));
            out.push_str(&w.finish());
            frees.push(format!("calloc.free({p});"));
            vec![p]
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let p = format!("{name}Ptr");
            let mut w = CodeWriter::two_space().with_depth(1);
            w.line(format!(
                "final {p} = {name}.isEmpty ? nullptr : calloc<Uint8>({name}.length);"
            ));
            w.line(format!(
                "for (var i = 0; i < {name}.length; i++) {{ {p}[i] = {name}[i]; }}"
            ));
            out.push_str(&w.finish());
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p, format!("{name}.length")]
        }
        TypeRef::Optional(inner) => emit_optional_input(out, name, inner, frees),
        TypeRef::List(inner) | TypeRef::Iterator(inner) => emit_list_input(out, name, inner, frees),
        TypeRef::Map(k, v) => emit_map_input(out, name, k, v, frees),
    }
}

fn emit_optional_input(
    out: &mut String,
    name: &str,
    inner: &TypeRef,
    frees: &mut Vec<String>,
) -> Vec<String> {
    let p = format!("{name}Ptr");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let mut w = CodeWriter::two_space().with_depth(1);
            w.line(format!(
                "final {p} = {name} == null ? nullptr : {name}.toNativeUtf8();"
            ));
            out.push_str(&w.finish());
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p]
        }
        TypeRef::TypedHandle(_) | TypeRef::Record(_) | TypeRef::RichEnum(_)
        | TypeRef::Interface(_) => {
            vec![format!("{name}?._handle ?? nullptr")]
        }
        other => {
            let (native, _) = scalar_ffi(other);
            let val = match other {
                TypeRef::Enum(_) => format!("{name}.value"),
                _ => name.to_string(),
            };
            let mut w = CodeWriter::two_space().with_depth(1);
            w.line(format!("Pointer<{native}> {p} = nullptr;"));
            w.line(format!("if ({name} != null) {{"));
            w.scope(|w| {
                w.line(format!("{p} = calloc<{native}>();"));
                w.line(format!("{p}.value = {val};"));
            });
            w.line("}");
            out.push_str(&w.finish());
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p]
        }
    }
}

fn emit_list_input(
    out: &mut String,
    name: &str,
    inner: &TypeRef,
    frees: &mut Vec<String>,
) -> Vec<String> {
    let p = format!("{name}Ptr");
    let arr_ty = input_array_ffi(inner);
    let inner_ffi = arr_ty
        .strip_prefix("Pointer<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or("Pointer<Void>")
        .to_string();
    let mut w = CodeWriter::two_space().with_depth(1);
    w.line(format!(
        "final {p} = {name}.isEmpty ? nullptr : calloc<{inner_ffi}>({name}.length);"
    ));
    w.line(format!("for (var i = 0; i < {name}.length; i++) {{"));
    w.scope(|w| {
        w.line(format!(
            "{p}[i] = {};",
            elem_to_native(&format!("{name}[i]"), inner)
        ));
    });
    w.line("}");
    out.push_str(&w.finish());
    if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        frees.push(format!(
            "if ({p} != nullptr) {{ for (var i = 0; i < {name}.length; i++) {{ calloc.free({p}[i]); }} calloc.free({p}); }}"
        ));
    } else {
        frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
    }
    vec![p, format!("{name}.length")]
}

fn emit_map_input(
    out: &mut String,
    name: &str,
    k: &TypeRef,
    v: &TypeRef,
    frees: &mut Vec<String>,
) -> Vec<String> {
    let kp = format!("{name}Keys");
    let vp = format!("{name}Vals");
    let kff = input_array_ffi(k);
    let vff = input_array_ffi(v);
    let ki = kff
        .strip_prefix("Pointer<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or("Pointer<Void>")
        .to_string();
    let vi = vff
        .strip_prefix("Pointer<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or("Pointer<Void>")
        .to_string();
    let mut w = CodeWriter::two_space().with_depth(1);
    w.line(format!("final {name}Entries = {name}.entries.toList();"));
    w.line(format!(
        "final {kp} = {name}.isEmpty ? nullptr : calloc<{ki}>({name}.length);"
    ));
    w.line(format!(
        "final {vp} = {name}.isEmpty ? nullptr : calloc<{vi}>({name}.length);"
    ));
    w.line(format!("for (var i = 0; i < {name}Entries.length; i++) {{"));
    w.scope(|w| {
        w.line(format!(
            "{kp}[i] = {};",
            elem_to_native(&format!("{name}Entries[i].key"), k)
        ));
        w.line(format!(
            "{vp}[i] = {};",
            elem_to_native(&format!("{name}Entries[i].value"), v)
        ));
    });
    w.line("}");
    out.push_str(&w.finish());
    let free_arr = |which: &str, ty: &TypeRef| -> String {
        if matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
            format!("if ({which} != nullptr) {{ for (var i = 0; i < {name}.length; i++) {{ calloc.free({which}[i]); }} calloc.free({which}); }}")
        } else {
            format!("if ({which} != nullptr) calloc.free({which});")
        }
    };
    frees.push(free_arr(&kp, k));
    frees.push(free_arr(&vp, v));
    vec![kp, vp, format!("{name}.length")]
}

/// Native expression converting a Dart element (`expr`) of a list/map into the
/// value stored in a native array slot.
fn elem_to_native(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{expr}.toNativeUtf8()"),
        TypeRef::TypedHandle(_) | TypeRef::Record(_) | TypeRef::RichEnum(_)
        | TypeRef::Interface(_) => {
            format!("{expr}._handle")
        }
        TypeRef::Enum(_) => format!("{expr}.value"),
        _ => expr.to_string(),
    }
}

/// Whether a return type lowers to callee-allocated out-parameters (so the call
/// wrapper must allocate `outLen`/`outKeys`/`outVals` and decode afterwards).
fn return_has_out_params(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Map(_, _)
    )
}

/// Whether an `optional<inner>` already lowers to a nullable pointer (so the
/// option is encoded by the pointer's nullness, not an extra indirection).
fn optional_inner_is_pointer(inner: &TypeRef) -> bool {
    matches!(
        inner,
        TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_)
    )
}

/// The FFI return type (native, dart) of a call symbol. Maps return-by-value via
/// out-params to a `void` symbol; an optional scalar lowers to a nullable
/// pointer-to-scalar; everything else follows [`native_ffi_type`].
fn return_ffi(ty: &TypeRef) -> (String, String) {
    match ty {
        TypeRef::Map(_, _) => ("Void".into(), "void".into()),
        TypeRef::Optional(inner) if !optional_inner_is_pointer(inner) => {
            let p = format!("Pointer<{}>", scalar_ffi(inner).0);
            (p.clone(), p)
        }
        _ => (native_ffi_type(ty), dart_ffi_type(ty)),
    }
}

/// dart:ffi type of the `outKeys`/`outValues` pointer passed *by address* for a
/// map return (the C slot is `K***`/`V***`).
fn map_out_ffi(elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Pointer<Pointer<Utf8>>>".into(),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => "Pointer<Pointer<Pointer<Void>>>".into(),
        _ => format!("Pointer<Pointer<{}>>", scalar_ffi(elem).0),
    }
}

/// Read one decoded array/map element (`arr` is the typed array pointer).
/// A string element is copied (the caller owes its release separately); an
/// object element is adopted by its wrapper class, whose `dispose()` owns the
/// eventual destroy. A `Bool` slot already reads back as a Dart `bool`.
fn map_elem_read(arr: &str, idx: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{arr}[{idx}].toDartString()"),
        TypeRef::Enum(n) => format!(
            "{}.fromValue({arr}[{idx}])",
            local_type_name(n).to_upper_camel_case()
        ),
        TypeRef::Record(n) | TypeRef::RichEnum(n) | TypeRef::TypedHandle(n)
        | TypeRef::Interface(n) => {
            format!(
                "{}._({arr}[{idx}])",
                local_type_name(n).to_upper_camel_case()
            )
        }
        _ => format!("{arr}[{idx}]"),
    }
}

/// The trailing FFI typedef slots (native, dart) a return type contributes for
/// its callee-allocated out-parameters.
fn return_out_slots(ty: &TypeRef) -> Vec<(String, String)> {
    let ptr = |s: String| (s.clone(), s);
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
            vec![ptr("Pointer<Size>".into())]
        }
        TypeRef::Map(k, v) => vec![
            ptr(map_out_ffi(k)),
            ptr(map_out_ffi(v)),
            ptr("Pointer<Size>".into()),
        ],
        _ => vec![],
    }
}

/// Allocate the out-parameter locals a complex return needs before the call,
/// returning the extra call-argument expressions and recording cleanup.
fn emit_return_alloc(
    out: &mut String,
    ty: &TypeRef,
    frees: &mut Vec<String>,
    indent: &str,
) -> Vec<String> {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let args = match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
            w.line("final outLen = calloc<Size>();");
            frees.push("calloc.free(outLen);".into());
            vec!["outLen".into()]
        }
        TypeRef::Map(k, v) => {
            let kf = map_out_ffi(k);
            let vf = map_out_ffi(v);
            // `outKeys`/`outValues` hold the array pointer the callee writes.
            let ki = kf
                .strip_prefix("Pointer<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap();
            let vi = vf
                .strip_prefix("Pointer<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap();
            w.line(format!("final outKeys = calloc<{ki}>();"));
            w.line(format!("final outValues = calloc<{vi}>();"));
            w.line("final outLen = calloc<Size>();");
            frees.push("calloc.free(outKeys);".into());
            frees.push("calloc.free(outValues);".into());
            frees.push("calloc.free(outLen);".into());
            vec!["outKeys".into(), "outValues".into(), "outLen".into()]
        }
        _ => vec![],
    };
    out.push_str(&w.finish());
    args
}

/// Emit the post-call decode of a (possibly complex) return into the wrapper's
/// Dart return value. `result` is the call result (absent for `void` map returns).
fn emit_return_decode(out: &mut String, ty: &TypeRef, indent: &str) {
    match ty {
        TypeRef::List(inner) => emit_list_conversion(out, inner, indent),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
            w.line("final n = outLen.value;");
            w.line("if (result == nullptr) return <int>[];");
            w.line("final bytes = List<int>.generate(n, (i) => result[i]);");
            // Copy first, then release the producer's buffer.
            w.line("_weaveffiFreeBytes(result, n);");
            w.line("return bytes;");
            out.push_str(&w.finish());
        }
        TypeRef::Map(k, v) => {
            let kt = dart_type(k);
            let vt = dart_type(v);
            let kp = elem_pointee(k);
            let vp = elem_pointee(v);
            let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
            w.line("final n = outLen.value;");
            w.line(format!("final m = <{kt}, {vt}>{{}};"));
            w.line("final keys = outKeys.value;");
            w.line("final vals = outValues.value;");
            w.line("for (var i = 0; i < n; i++) {");
            w.scope(|w| {
                w.line(format!(
                    "m[{}] = {};",
                    map_elem_read("keys", "i", k),
                    map_elem_read("vals", "i", v)
                ));
            });
            w.line("}");
            // Release each copied string element, then both parallel arrays.
            for (arr, ty) in [("keys", k), ("vals", v)] {
                if matches!(ty.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
                    w.line("for (var i = 0; i < n; i++) {");
                    w.scope(|w| {
                        w.line(format!("_weaveffiFreeString({arr}[i]);"));
                    });
                    w.line("}");
                }
            }
            w.line("if (keys != nullptr) {");
            w.scope(|w| {
                w.line(format!(
                    "_weaveffiFreeBytes(keys.cast(), n * sizeOf<{kp}>());"
                ));
            });
            w.line("}");
            w.line("if (vals != nullptr) {");
            w.scope(|w| {
                w.line(format!(
                    "_weaveffiFreeBytes(vals.cast(), n * sizeOf<{vp}>());"
                ));
            });
            w.line("}");
            w.line("return m;");
            out.push_str(&w.finish());
        }
        _ => emit_result_conversion(out, ty, indent),
    }
}

/// Convert a single native leaf value (`expr`) into its Dart representation.
/// A `Bool` slot already reads back as a Dart `bool`.
fn read_value(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{expr}.toDartString()"),
        TypeRef::Enum(n) => format!(
            "{}.fromValue({expr})",
            local_type_name(n).to_upper_camel_case()
        ),
        TypeRef::Record(n) | TypeRef::RichEnum(n) | TypeRef::TypedHandle(n)
        | TypeRef::Interface(n) => {
            format!("{}._({expr})", local_type_name(n).to_upper_camel_case())
        }
        _ => expr.to_string(),
    }
}

/// dart:ffi pointee type of one element slot: an iterator's `out_item`
/// allocation, a returned array's element, or a returned map's key/value
/// element (the C slot is `T*`).
fn elem_pointee(elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => "Pointer<Void>".into(),
        _ => scalar_ffi(elem).0.to_string(),
    }
}

fn emit_typedef_and_lookup(
    out: &mut String,
    c_sym: &str,
    native_params: &str,
    dart_params: &str,
    native_ret: &str,
    dart_ret: &str,
) {
    let td = c_sym.to_upper_camel_case();
    let var = c_sym.to_lower_camel_case();
    out.push_str(&format!(
        "\ntypedef _Native{td} = {native_ret} Function({native_params});\n"
    ));
    out.push_str(&format!(
        "typedef _Dart{td} = {dart_ret} Function({dart_params});\n"
    ));
    out.push_str(&format!(
        "final _{var} = _lib.lookupFunction<\n    _Native{td}, _Dart{td}>('{c_sym}');\n"
    ));
}

fn render_pubspec(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "pubspec.yaml");
    let name = package.ident_name();
    let version = &package.version;
    let description = package.description_or_default();
    let mut meta = format!("description: {description}\n");
    if let Some(homepage) = package.homepage.as_ref().or(package.repository.as_ref()) {
        meta.push_str(&format!("homepage: {homepage}\n"));
    }
    format!(
        "{prelude}name: {name}\n\
         version: {version}\n\
         {meta}\
         environment:\n\
         \x20 sdk: '>=3.0.0 <4.0.0'\n\
         dependencies:\n\
         \x20 ffi: ^2.0.0\n\n\
         {trailer}"
    )
}

fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let import_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Dart)

Auto-generated Dart bindings using `dart:ffi`.

## Usage

1. Place the compiled shared library (`libweaveffi.dylib`, `libweaveffi.so`,
   or `weaveffi.dll`) where the Dart process can find it.

2. Add this package as a dependency and import the bindings:

```dart
import 'package:{import_name}/weaveffi.dart';
```

3. Call the generated functions directly. The bindings use `dart:ffi` to load
   the native library at runtime via `DynamicLibrary.open` and resolve symbols
   with `lookupFunction`.

## Requirements

- Dart SDK >= 3.0.0
- The `ffi` package (`^2.0.0`) for `Utf8` and `calloc` helpers.

{trailer}"#
    )
}

fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::TripleSlash);
}

fn render_dart_module(api: &Api, model: &BindingModel, config: &DartConfig) -> String {
    let input_basename = config.input_basename();
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    // The default shared-library basename follows the package identity
    // (`lib<name>`), matching the producer cdylib. WEAVEFFI_LIBRARY still wins.
    let resolved = pkg::resolve(api, None, Some(input_basename));
    let lib_base = resolved.ident_name();

    out.push_str(
        "// ignore_for_file: non_constant_identifier_names, camel_case_types, unused_element\n\n",
    );
    out.push_str("import 'dart:ffi';\n");
    out.push_str("import 'dart:io' show Platform;\n");
    if has_async {
        out.push_str("import 'dart:async';\n");
    }
    out.push_str("import 'package:ffi/ffi.dart';\n\n");

    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str("  // An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a\n");
    out.push_str("  // specific build artifact regardless of its file name or location.\n");
    out.push_str("  final override = Platform.environment['WEAVEFFI_LIBRARY'];\n");
    out.push_str(
        "  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);\n",
    );
    out.push_str(&format!(
        "  if (Platform.isMacOS) return DynamicLibrary.open('lib{lib_base}.dylib');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isLinux) return DynamicLibrary.open('lib{lib_base}.so');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isWindows) return DynamicLibrary.open('{lib_base}.dll');\n"
    ));
    out.push_str(
        "  throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');\n",
    );
    out.push_str("}\n\n");
    out.push_str("final DynamicLibrary _lib = _openLibrary();\n\n");

    out.push_str("final class _WeaveFFIError extends Struct {\n");
    out.push_str("  @Int32()\n");
    out.push_str("  external int code;\n");
    out.push_str("  external Pointer<Utf8> message;\n");
    out.push_str("}\n");

    emit_typedef_and_lookup(
        &mut out,
        "weaveffi_error_clear",
        "Pointer<_WeaveFFIError>",
        "Pointer<_WeaveFFIError>",
        "Void",
        "void",
    );

    // Runtime release helpers: every returned `const char*` is freed with
    // `weaveffi_free_string` after copying, and every producer-allocated
    // buffer (bytes, array, map, boxed optional scalar) with
    // `weaveffi_free_bytes`. The runtime always exports these under the
    // canonical `weaveffi_` names, like `weaveffi_error_clear`.
    emit_typedef_and_lookup(
        &mut out,
        "weaveffi_free_string",
        "Pointer<Utf8>",
        "Pointer<Utf8>",
        "Void",
        "void",
    );
    emit_typedef_and_lookup(
        &mut out,
        "weaveffi_free_bytes",
        "Pointer<Uint8>, Size",
        "Pointer<Uint8>, int",
        "Void",
        "void",
    );

    out.push_str(
        "\n/// Generic WeaveFFI failure: panics, marshalling errors, and unknown codes.\n",
    );
    out.push_str("class WeaveFFIException implements Exception {\n");
    out.push_str("  final int code;\n");
    out.push_str("  final String message;\n");
    out.push_str("  WeaveFFIException(this.code, this.message);\n");
    out.push_str("  @override\n");
    out.push_str("  String toString() => '$runtimeType($code): $message';\n");
    out.push_str("}\n\n");

    out.push_str("void _checkError(Pointer<_WeaveFFIError> err) {\n");
    out.push_str("  if (err.ref.code != 0) {\n");
    // Capture code and message *before* clearing, which zeroes the struct.
    out.push_str("    final code = err.ref.code;\n");
    out.push_str("    final msg = err.ref.message.toDartString();\n");
    out.push_str("    _weaveffiErrorClear(err);\n");
    out.push_str("    throw WeaveFFIException(code, msg);\n");
    out.push_str("  }\n");
    out.push_str("}\n");

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        out.push_str("\n// Live listener trampolines by subscription id. Holding the\n");
        out.push_str("// NativeCallable here keeps its native thunk alive until unregistered.\n");
        out.push_str("final Map<int, NativeCallable> _listenerCallables = {};\n");
    }

    let has_iterators = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| matches!(f.shape, CallShape::Iterator(_))));
    if has_iterators {
        out.push_str("\n// Anchors one live native iteration for its GC-finalizer backstop.\n");
        out.push_str("// A suspended `sync*` frame keeps the anchor reachable; abandoning the\n");
        out.push_str("// iteration drops the frame, and the finalizer destroys the native\n");
        out.push_str("// iterator handle. Exhausted iterations detach before destroying\n");
        out.push_str("// eagerly, so the handle is destroyed exactly once either way.\n");
        out.push_str("final class _IteratorLifetime implements Finalizable {}\n");
    }

    // Canonical member order per module: error domain, enums, structs,
    // interfaces, callbacks, listeners, functions.
    for module in &model.modules {
        if let Some(eb) = module.error.as_ref().filter(|e| e.declared_here) {
            render_error(&mut out, module, eb);
        }
        for e in &module.enums {
            render_enum(&mut out, e);
        }
        for s in &module.structs {
            render_struct(&mut out, s);
            if s.builder.is_some() {
                render_dart_builder(&mut out, s);
            }
        }
        for i in &module.interfaces {
            render_interface(&mut out, module, i, &model.prefix);
        }
        for cb in &module.callbacks {
            render_callback_typedef(&mut out, cb);
        }
        for l in &module.listeners {
            render_listener(&mut out, module, l, config.strip_module_prefix);
        }
        for f in &module.functions {
            render_function(&mut out, module, f, config.strip_module_prefix, &model.prefix);
        }
    }

    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi.dart"));
    out
}

/// The Dart exception class named by an error domain or one of its codes: the
/// PascalCase name with a trailing `Error` swapped for `Exception`, so
/// `KvError` becomes `KvException` and a code `IoError` becomes `IoException`.
fn dart_exception_name(raw: &str) -> String {
    errors::exception_type_name(raw)
}

/// Escape a string for embedding in a single-quoted Dart literal.
fn dart_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('$', "\\$")
}

/// Error-reporting context for one wrapper: which check helper guards its
/// out-err slot and which exception its async completion path constructs.
///
/// The split follows [`ErrorStrategy`]: a throwing callable maps `out_err`
/// onto the module's typed domain exception, while a non-throwing callable
/// traps through the generic brand exception (a reported error there is only
/// ever a producer bug, never a domain error).
#[derive(Clone, Copy)]
struct ErrCtx<'a> {
    /// `true` when the wrapper surfaces typed domain errors (`throws: true`).
    throws: bool,
    /// The domain exception class in effect (`KvException` names `_checkKvException`
    /// and `_mapKvException`); `None` when no error domain is in scope.
    exception: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// The domain exception this wrapper throws, or `None` for a non-throwing
    /// wrapper (which reports every failure as the generic brand exception).
    fn thrown_exception(&self) -> Option<&'a str> {
        self.exception.filter(|_| self.throws)
    }

    /// The statement checking the wrapper's `err` slot after a call.
    fn check_stmt(&self) -> String {
        match self.thrown_exception() {
            Some(exc) => format!("_check{exc}(err);"),
            None => "_checkError(err);".to_string(),
        }
    }

    /// The expression building the exception for an async completion's
    /// already-captured `code`/`msg` locals.
    fn map_expr(&self) -> String {
        match self.thrown_exception() {
            Some(exc) => format!("_map{exc}(code, msg)"),
            None => "WeaveFFIException(code, msg)".to_string(),
        }
    }
}

/// Render one module's declared error domain: the domain exception extending
/// the generic [`errors::EXCEPTION_BRAND`], one exception subclass per code
/// carrying its stable code and default message, and the `_map`/`_check`
/// helpers that throwing wrappers route their out-err slots through. Unknown
/// codes (panics, marshalling failures) fall back to the generic exception.
fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let exc = dart_exception_name(&eb.type_name);
    let brand = errors::EXCEPTION_BRAND;

    let mut w = CodeWriter::two_space();
    w.blank();
    w.line(format!(
        "/// Typed error domain `{}` declared by module `{}`.",
        eb.name, module.path
    ));
    w.block(format!("class {exc} extends {brand} {{"), "}", |w| {
        w.line(format!("{exc}(super.code, super.message);"));
    });

    for c in &eb.codes {
        let class = dart_exception_name(&c.name);
        let message = dart_str_literal(&c.message);
        w.blank();
        let doc = c.doc.clone().or_else(|| Some(c.message.clone()));
        {
            let mut d = String::new();
            emit_doc(&mut d, &doc, "");
            w.raw(d);
        }
        w.block(format!("class {class} extends {exc} {{"), "}", |w| {
            w.line(format!(
                "{class}([String message = '{message}']) : super({}, message);",
                c.value
            ));
        });
    }

    w.blank();
    w.line(format!("{brand} _map{exc}(int code, String message) {{"));
    w.scope(|w| {
        w.block("switch (code) {", "}", |w| {
            for c in &eb.codes {
                w.line(format!("case {}:", c.value));
                w.scope(|w| {
                    w.line(format!("return {}(message);", dart_exception_name(&c.name)));
                });
            }
            w.line("default:");
            w.scope(|w| {
                w.line(format!("return {brand}(code, message);"));
            });
        });
    });
    w.line("}");

    w.blank();
    w.block(
        format!("void _check{exc}(Pointer<_WeaveFFIError> err) {{"),
        "}",
        |w| {
            w.block("if (err.ref.code != 0) {", "}", |w| {
                w.line("final code = err.ref.code;");
                w.line("final msg = err.ref.message.toDartString();");
                w.line("_weaveffiErrorClear(err);");
                w.line(format!("throw _map{exc}(code, msg);"));
            });
        },
    );
    out.push_str(&w.finish());
}

/// The [`ErrCtx`] for one callable of `module`: its [`ErrorStrategy`] paired
/// with the exception class of the domain in effect (own or inherited).
fn err_ctx<'a>(f: &FnBinding, exception: Option<&'a str>) -> ErrCtx<'a> {
    ErrCtx {
        throws: matches!(f.error_strategy(), ErrorStrategy::Throws),
        exception,
    }
}

/// Render one interface as an opaque-object wrapper class, mirroring the Dart
/// struct wrapper: it owns the C handle behind a private `_handle`, frees it
/// once in `dispose()` via the interface's destroy symbol, and exposes the
/// canonical `new` constructor as an unnamed factory (`Store(...)`), every
/// other constructor as a named factory (`Store.open(...)`), instance methods
/// that pass `_handle` as the implicit leading FFI argument, and statics as
/// `static` methods. Member FFI typedefs and lookups stay at file scope.
fn render_interface(out: &mut String, module: &ModuleBinding, i: &InterfaceBinding, prefix: &str) {
    let class_name = i.name.to_upper_camel_case();
    emit_typedef_and_lookup(
        out,
        &i.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    let exc = module
        .error
        .as_ref()
        .map(|e| dart_exception_name(&e.type_name));

    // Members render exactly like free functions (depth 0), with the lookups
    // going to file scope and the declarations collected for the class body.
    let mut members = String::new();
    for c in &i.constructors {
        let kind = DartDecl::Factory {
            class_name: &class_name,
            named: c.name != "new",
        };
        render_callable(
            out,
            &mut members,
            c,
            &kind,
            &c.name.to_lower_camel_case(),
            err_ctx(c, exc.as_deref()),
            module,
            prefix,
        );
    }
    for m in &i.methods {
        render_callable(
            out,
            &mut members,
            m,
            &DartDecl::Method,
            &m.name.to_lower_camel_case(),
            err_ctx(m, exc.as_deref()),
            module,
            prefix,
        );
    }
    for s in &i.statics {
        render_callable(
            out,
            &mut members,
            s,
            &DartDecl::Static,
            &s.name.to_lower_camel_case(),
            err_ctx(s, exc.as_deref()),
            module,
            prefix,
        );
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &i.doc, "");
        w.raw(d);
    }
    w.block(format!("class {class_name} {{"), "}", |w| {
        w.line("final Pointer<Void> _handle;");
        w.line(format!("{class_name}._(this._handle);"));
        w.blank();
        w.line("/// Releases the native object reference.");
        w.block("void dispose() {", "}", |w| {
            w.line(format!(
                "_{}(_handle);",
                i.destroy_symbol.to_lower_camel_case()
            ));
        });
        // Reindent the depth-0 member declarations into the class body.
        w.block_raw(&members);
    });
    out.push_str(&w.finish());
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum crosses the ABI as an opaque object, so it is
    // emitted as a wrapper class (like a struct), not a plain Dart `enum`.
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    let name = e.name.to_upper_camel_case();
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("enum {name} {{"), "}", |w| {
        for v in &e.variants {
            let vname = v.name.to_lower_camel_case();
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "  ");
            w.raw(vd);
            w.line(format!("{vname}({}),", v.value));
        }
        w.line(";");
        w.line(format!("const {name}(this.value);"));
        w.line("final int value;");
        w.blank();
        w.line(format!(
            "static {name} fromValue(int value) =>\n      {name}.values.firstWhere((e) => e.value == value);"
        ));
    });
    out.push_str(&w.finish());
}

fn render_struct(out: &mut String, s: &StructBinding) {
    let class_name = s.name.to_upper_camel_case();
    // Symbols come precomputed from the shared BindingModel, so Dart never
    // re-derives the `{prefix}_{module}_{Name}_*` scheme itself.
    let destroy_sym = &s.destroy_symbol;
    emit_typedef_and_lookup(
        out,
        destroy_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    for field in &s.fields {
        emit_field_getter_typedef(out, field);
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &s.doc, "");
        w.raw(d);
    }
    w.block(format!("class {class_name} {{"), "}", |w| {
        w.line("final Pointer<Void> _handle;");
        w.line(format!("{class_name}._(this._handle);"));
        w.blank();
        w.block("void dispose() {", "}", |w| {
            w.line(format!("_{}(_handle);", destroy_sym.to_lower_camel_case()));
        });
        for field in &s.fields {
            let mut m = String::new();
            emit_field_getter_method(&mut m, field);
            w.raw(m);
        }
    });
    out.push_str(&w.finish());
}

/// Emit the dart:ffi typedef + `lookupFunction` for one opaque-object field
/// getter (a struct field or a rich-enum variant field). The getter takes only
/// the opaque handle and reports no error; a bytes/list field adds its
/// callee-allocated out-param and a map field adds its triple and lowers to a
/// `void` symbol. The lookup is keyed on the field's precomputed
/// `getter_symbol`, so a rich enum may rename the Dart member freely.
fn emit_field_getter_typedef(out: &mut String, field: &FieldBinding) {
    let getter_sym = &field.getter_symbol;
    let mut nparams = vec!["Pointer<Void>".to_string()];
    let mut dparams = vec!["Pointer<Void>".to_string()];
    for (n, d) in return_out_slots(&field.ty) {
        nparams.push(n);
        dparams.push(d);
    }
    let (nr, dr) = return_ffi(&field.ty);
    emit_typedef_and_lookup(
        out,
        getter_sym,
        &nparams.join(", "),
        &dparams.join(", "),
        &nr,
        &dr,
    );
}

/// Emit the idiomatic Dart getter for one opaque-object field. The member name
/// comes from `field.name`, so a rich enum can namespace it per variant (e.g.
/// `circleRadius`) by passing a renamed [`FieldBinding`], while the FFI lookup
/// stays keyed on the precomputed `getter_symbol`. Receiver is the wrapper's
/// `_handle`, common to both the struct and rich-enum classes.
fn emit_field_getter_method(out: &mut String, field: &FieldBinding) {
    let getter_sym = &field.getter_symbol;
    let dart_ret = dart_type(&field.ty);
    let fname = field.name.to_lower_camel_case();

    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &field.doc, "  ");
        w.raw(d);
    }
    w.line(format!("{dart_ret} get {fname} {{"));
    if return_has_out_params(&field.ty) {
        let mut frees: Vec<String> = Vec::new();
        let mut args = vec!["_handle".to_string()];
        let mut alloc = String::new();
        args.extend(emit_return_alloc(&mut alloc, &field.ty, &mut frees, "    "));
        let mut dec = String::new();
        emit_return_decode(&mut dec, &field.ty, "      ");
        w.raw(alloc);
        w.scope(|w| {
            w.line("try {");
            w.scope(|w| {
                if matches!(&field.ty, TypeRef::Map(_, _)) {
                    w.line(format!(
                        "_{}({});",
                        getter_sym.to_lower_camel_case(),
                        args.join(", ")
                    ));
                } else {
                    w.line(format!(
                        "final result = _{}({});",
                        getter_sym.to_lower_camel_case(),
                        args.join(", ")
                    ));
                }
                w.raw(&dec);
            });
            w.line("} finally {");
            w.scope(|w| {
                for fr in &frees {
                    w.line(fr);
                }
            });
            w.line("}");
        });
    } else {
        let mut conv = String::new();
        emit_result_conversion(&mut conv, &field.ty, "    ");
        w.scope(|w| {
            w.line(format!(
                "final result = _{}(_handle);",
                getter_sym.to_lower_camel_case()
            ));
            w.raw(conv);
        });
    }
    w.line("}");
    out.push_str(&w.finish());
}

fn render_dart_builder(out: &mut String, s: &StructBinding) {
    let class_name = s.name.to_upper_camel_case();
    let builder_name = format!("{class_name}Builder");
    let create_sym = &s.create.symbol;

    // `{Struct}_create(<field slots>, error* out_err) -> {Struct}*`: each field
    // expands to its ABI slots, then the trailing error pointer.
    let mut nparams: Vec<String> = Vec::new();
    let mut dparams: Vec<String> = Vec::new();
    for field in &s.fields {
        for (n, d) in input_slots(&field.ty) {
            nparams.push(n);
            dparams.push(d);
        }
    }
    nparams.push("Pointer<_WeaveFFIError>".into());
    dparams.push("Pointer<_WeaveFFIError>".into());
    emit_typedef_and_lookup(
        out,
        create_sym,
        &nparams.join(", "),
        &dparams.join(", "),
        "Pointer<Void>",
        "Pointer<Void>",
    );

    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    let mut staging = String::new();
    for field in &s.fields {
        let args = emit_input(
            &mut staging,
            &field.name.to_lower_camel_case(),
            &field.ty,
            &mut frees,
        );
        call_args.extend(args);
    }
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &s.doc, "");
        w.raw(d);
    }
    w.block(format!("class {builder_name} {{"), "}", |w| {
        for field in &s.fields {
            let dt = dart_nullable_type_for_builder_field(&field.ty);
            let priv_name = field.name.to_lower_camel_case();
            w.line(format!("{dt} _{priv_name};"));
        }

        for field in &s.fields {
            let pascal = field.name.to_upper_camel_case();
            let dt = dart_type(&field.ty);
            let priv_name = field.name.to_lower_camel_case();
            w.blank();
            {
                let mut fd = String::new();
                emit_doc(&mut fd, &field.doc, "  ");
                w.raw(fd);
            }
            w.block(
                format!("{builder_name} with{pascal}({dt} value) {{"),
                "}",
                |w| {
                    w.line(format!("_{priv_name} = value;"));
                    w.line("return this;");
                },
            );
        }

        w.blank();
        w.block(format!("{class_name} build() {{"), "}", |w| {
            // Required fields must be set; optional fields default to null.
            for field in &s.fields {
                if !matches!(&field.ty, TypeRef::Optional(_)) {
                    let priv_name = field.name.to_lower_camel_case();
                    w.block(format!("if (_{priv_name} == null) {{"), "}", |w| {
                        w.line(format!(
                            "throw StateError('missing field: {}');",
                            field.name
                        ));
                    });
                }
            }
            for field in &s.fields {
                let priv_name = field.name.to_lower_camel_case();
                if matches!(&field.ty, TypeRef::Optional(_)) {
                    w.line(format!("final {priv_name} = _{priv_name};"));
                } else {
                    w.line(format!("final {priv_name} = _{priv_name}!;"));
                }
            }
            w.raw(&staging);
            w.line("final err = calloc<_WeaveFFIError>();");
            w.line("try {");
            w.scope(|w| {
                w.line(format!(
                    "final result = _{}({});",
                    create_sym.to_lower_camel_case(),
                    call_args.join(", ")
                ));
                w.line("_checkError(err);");
                w.line(format!("return {class_name}._(result);"));
            });
            w.line("} finally {");
            w.scope(|w| {
                for fr in &frees {
                    w.line(fr);
                }
            });
            w.line("}");
        });
    });
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an opaque-object wrapper, mirroring the
/// Dart struct wrapper: it owns the C handle behind a private `_handle` (so the
/// existing function marshalling, `x._handle` in, `Name._(result)` out, keeps
/// working unchanged, since a `TypeRef::RichEnum` reference shares the
/// record's opaque-pointer ABI), frees it
/// once in `dispose()`, and exposes a `tag` discriminant reader, one `factory`
/// per variant (`Shape.circle(2.5)`), and per-variant field getters namespaced
/// by variant (`circleRadius`) to avoid collisions. The opaque-object surface
/// (tag/destroy symbols, per-variant constructors and field getters) is
/// precomputed in the binding model exactly like a struct's.
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let rich = e
        .rich
        .as_ref()
        .expect("render_rich_enum requires a rich (algebraic) enum");
    let class_name = e.name.to_upper_camel_case();
    let tag_name = format!("{class_name}Tag");

    // A top-level discriminant enum (`ShapeTag.circle`), rendered exactly like a
    // plain enum so the active variant reads back as a typed value.
    render_rich_enum_tag(out, e, &tag_name);

    // FFI typedefs + lookups, keyed on the model's precomputed symbols: the
    // destructor, the tag getter, one constructor per variant, and every
    // per-variant field getter.
    emit_typedef_and_lookup(
        out,
        &rich.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
    emit_typedef_and_lookup(
        out,
        &rich.tag_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Int32",
        "int",
    );
    for v in &rich.variants {
        emit_rich_variant_create_typedef(out, v);
    }
    for v in &rich.variants {
        for field in namespaced_variant_fields(v) {
            emit_field_getter_typedef(out, &field);
        }
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("class {class_name} {{"), "}", |w| {
        w.line("final Pointer<Void> _handle;");
        w.line(format!("{class_name}._(this._handle);"));
        w.blank();
        w.block("void dispose() {", "}", |w| {
            w.line(format!(
                "_{}(_handle);",
                rich.destroy_symbol.to_lower_camel_case()
            ));
        });

        // The active variant's discriminant, read back as the typed tag enum.
        w.blank();
        w.line(format!(
            "{tag_name} get tag =>\n      {tag_name}.fromValue(_{}(_handle));",
            rich.tag_symbol.to_lower_camel_case()
        ));

        // One factory constructor per variant (`Shape.circle(2.5)`).
        for v in &rich.variants {
            let mut m = String::new();
            emit_rich_variant_factory(&mut m, &class_name, v);
            w.raw(m);
        }

        // Per-variant field getters, namespaced by variant (`circleRadius`).
        for v in &rich.variants {
            for field in namespaced_variant_fields(v) {
                let mut m = String::new();
                emit_field_getter_method(&mut m, &field);
                w.raw(m);
            }
        }
    });
    out.push_str(&w.finish());
}

/// The typed discriminant of a rich enum, emitted as a top-level Dart `enum`
/// (Dart cannot nest an `enum` in a class). Mirrors [`render_enum`]'s enhanced
/// enum so `tag` reads back as e.g. `ShapeTag.circle`.
fn render_rich_enum_tag(out: &mut String, e: &EnumBinding, tag_name: &str) {
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("enum {tag_name} {{"), "}", |w| {
        for v in &e.variants {
            let vname = v.name.to_lower_camel_case();
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "  ");
            w.raw(vd);
            w.line(format!("{vname}({}),", v.value));
        }
        w.line(";");
        w.line(format!("const {tag_name}(this.value);"));
        w.line("final int value;");
        w.blank();
        w.line(format!(
            "static {tag_name} fromValue(int value) =>\n      {tag_name}.values.firstWhere((e) => e.value == value);"
        ));
    });
    out.push_str(&w.finish());
}

/// Project a variant's fields into [`FieldBinding`]s whose Dart member name is
/// namespaced by the variant (`circle` + `radius` -> `circle_radius`, rendered
/// `circleRadius`). The precomputed `getter_symbol` is left untouched, so the
/// FFI lookup still targets the correct per-variant C symbol; this is what lets
/// the rich enum reuse the struct field-getter renderers verbatim.
fn namespaced_variant_fields(v: &RichVariantBinding) -> Vec<FieldBinding> {
    let variant = v.name.to_snake_case();
    v.fields
        .iter()
        .map(|f| {
            let mut namespaced = f.clone();
            namespaced.name = format!("{variant}_{}", f.name);
            namespaced
        })
        .collect()
}

/// Emit the dart:ffi typedef + lookup for one variant constructor
/// (`{c_tag}_{Variant}_new`): each variant field lowers to its ABI input slots,
/// then a trailing `out_err`; the call returns the opaque handle. Mirrors the
/// struct builder's `create` typedef.
fn emit_rich_variant_create_typedef(out: &mut String, v: &RichVariantBinding) {
    let create_sym = &v.create.symbol;
    let mut nparams: Vec<String> = Vec::new();
    let mut dparams: Vec<String> = Vec::new();
    for f in &v.fields {
        for (n, d) in input_slots(&f.ty) {
            nparams.push(n);
            dparams.push(d);
        }
    }
    nparams.push("Pointer<_WeaveFFIError>".into());
    dparams.push("Pointer<_WeaveFFIError>".into());
    emit_typedef_and_lookup(
        out,
        create_sym,
        &nparams.join(", "),
        &dparams.join(", "),
        "Pointer<Void>",
        "Pointer<Void>",
    );
}

/// Emit one variant's factory constructor (`Shape.circle(double radius)`).
/// Mirrors the struct builder's `build()`: each field marshals to its ABI
/// argument slots via [`emit_input`], the call threads an `out_err` checked with
/// `_checkError`, and the returned handle is wrapped (`return Shape._(result)`).
/// A unit variant takes no parameters and passes only the error slot.
fn emit_rich_variant_factory(out: &mut String, class_name: &str, v: &RichVariantBinding) {
    let create_sym = &v.create.symbol;
    let factory = v.name.to_lower_camel_case();
    let params: Vec<String> = v
        .fields
        .iter()
        .map(|f| format!("{} {}", dart_type(&f.ty), f.name.to_lower_camel_case()))
        .collect();

    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    let mut staging = String::new();
    for f in &v.fields {
        let args = emit_input(
            &mut staging,
            &f.name.to_lower_camel_case(),
            &f.ty,
            &mut frees,
        );
        call_args.extend(args);
    }
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &v.doc, "  ");
        w.raw(d);
    }
    w.line(format!(
        "factory {class_name}.{factory}({}) {{",
        params.join(", ")
    ));
    w.raw(staging);
    w.scope(|w| {
        w.line("final err = calloc<_WeaveFFIError>();");
        w.line("try {");
        w.scope(|w| {
            w.line(format!(
                "final result = _{}({});",
                create_sym.to_lower_camel_case(),
                call_args.join(", ")
            ));
            w.line("_checkError(err);");
            w.line(format!("return {class_name}._(result);"));
        });
        w.line("} finally {");
        w.scope(|w| {
            for fr in &frees {
                w.line(fr);
            }
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// How one rendered wrapper is declared in Dart source: a top-level function,
/// or a member (method, static, or factory constructor) of an interface class.
enum DartDecl<'a> {
    /// A top-level free function.
    TopLevel,
    /// An instance method of an interface class: the FFI call passes the
    /// wrapper's `_handle` as the implicit leading argument.
    Method,
    /// A `static` method of an interface class.
    Static,
    /// A `factory` constructor of the interface class. `named` is `false` for
    /// the canonical `new` constructor (`factory Store(...)`) and `true` for
    /// every other constructor (`factory Store.open(...)`).
    Factory { class_name: &'a str, named: bool },
}

impl DartDecl<'_> {
    /// The declaration's opening line (through the `{`). `ret` is the public
    /// return type, already wrapped in `Future<...>` for an async member.
    fn open_line(&self, ret: &str, name: &str, params: &str) -> String {
        match self {
            DartDecl::TopLevel | DartDecl::Method => format!("{ret} {name}({params}) {{"),
            DartDecl::Static => format!("static {ret} {name}({params}) {{"),
            DartDecl::Factory {
                class_name,
                named: false,
            } => format!("factory {class_name}({params}) {{"),
            DartDecl::Factory {
                class_name,
                named: true,
            } => format!("factory {class_name}.{name}({params}) {{"),
        }
    }

    /// The opening line of a `sync*` generator wrapper (an `iter<T>` return).
    /// Constructors never return iterators, so no factory spelling exists.
    fn open_line_sync_star(&self, ret: &str, name: &str, params: &str) -> String {
        match self {
            DartDecl::TopLevel | DartDecl::Method => format!("{ret} {name}({params}) sync* {{"),
            DartDecl::Static => format!("static {ret} {name}({params}) sync* {{"),
            DartDecl::Factory { .. } => {
                unreachable!("constructors cannot return iterators")
            }
        }
    }
}

fn render_function(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    strip: bool,
    prefix: &str,
) {
    let name = wrapper_name(&module.path, &f.name, strip).to_lower_camel_case();
    let exc = module
        .error
        .as_ref()
        .map(|e| dart_exception_name(&e.type_name));
    let mut decl = String::new();
    render_callable(
        out,
        &mut decl,
        f,
        &DartDecl::TopLevel,
        &name,
        err_ctx(f, exc.as_deref()),
        module,
        prefix,
    );
    out.push_str(&decl);
}

/// Render one callable: its FFI typedefs and lookups into `lookups` (always
/// top-level) and its Dart wrapper declaration into `decl` (top-level for a
/// free function, spliced into the class body for an interface member).
/// `module` and `prefix` locate the callable for the plan's per-element
/// release decisions (an iterator's [`ElemFree`]).
#[allow(clippy::too_many_arguments)]
fn render_callable(
    lookups: &mut String,
    decl: &mut String,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
    module: &ModuleBinding,
    prefix: &str,
) {
    // `c_base` is the prefixed `{prefix}_{module}_{name}` symbol the shared
    // BindingModel already computed; the async/iterator suffixing matches the C
    // ABI by construction.
    let c_sym = f.c_base.as_str();
    let pub_ret = f.ret.as_ref().map_or("void".into(), dart_type);
    let wrapper_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), p.name.to_lower_camel_case()))
        .collect();

    if f.is_async {
        render_async_function(
            lookups,
            decl,
            c_sym,
            f,
            kind,
            name,
            &pub_ret,
            &wrapper_params,
            err,
        );
        return;
    }

    // Each input parameter expands to its ABI slots (bytes/list/map fan out to
    // `(ptr, len)` / `(keys, vals, len)`); a complex return adds its callee-
    // allocated out-params; the trailing error slot closes the signature. An
    // instance method's `AbiFn` carries an implicit leading `self` pointer.
    let mut native_params: Vec<String> = Vec::new();
    let mut dart_params: Vec<String> = Vec::new();
    if f.has_self {
        native_params.push("Pointer<Void>".into());
        dart_params.push("Pointer<Void>".into());
    }
    for p in &f.params {
        for (n, d) in input_slots(&p.ty) {
            native_params.push(n);
            dart_params.push(d);
        }
    }
    if let Some(ret) = &f.ret {
        for (n, d) in return_out_slots(ret) {
            native_params.push(n);
            dart_params.push(d);
        }
    }
    native_params.push("Pointer<_WeaveFFIError>".into());
    dart_params.push("Pointer<_WeaveFFIError>".into());

    let (native_ret, dart_ret) = match &f.ret {
        Some(ret) => return_ffi(ret),
        None => ("Void".into(), "void".into()),
    };

    emit_typedef_and_lookup(
        lookups,
        c_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        &native_ret,
        &dart_ret,
    );

    // Iterator-returning functions also bind the element `next`/`destroy`
    // symbols plus the GC-finalizer backstop for abandoned iterations.
    if let CallShape::Iterator(ib) = &f.shape {
        emit_iter_lookups(lookups, ib);
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    emit_wrapper_doc(&mut w, f, err);
    let params = wrapper_params.join(", ");
    if let CallShape::Iterator(ib) = &f.shape {
        // The wrapper is a lazy `sync*` generator; everything (staging,
        // launch, per-element pulls, cleanup) lives in the generator body.
        w.line(kind.open_line_sync_star(&pub_ret, name, &params));
        let mut body = String::new();
        emit_iterator_body(&mut body, f, c_sym, ib, err, module, prefix);
        w.raw(body);
    } else {
        w.line(kind.open_line(&pub_ret, name, &params));
        let mut body = String::new();
        emit_function_body(&mut body, f, c_sym, err);
        w.raw(body);
    }
    w.line("}");
    decl.push_str(&w.finish());
}

/// Emit a wrapper's doc comment, the streaming/disposal note for an iterator
/// callable, the typed-exception note for a throwing callable, and its
/// `@Deprecated` annotation when present.
fn emit_wrapper_doc(w: &mut CodeWriter, f: &FnBinding, err: ErrCtx) {
    {
        let mut d = String::new();
        emit_doc(&mut d, &f.doc, "");
        w.raw(d);
    }
    let mut has_content = f.doc.is_some();
    let separator = |w: &mut CodeWriter, has_content: &mut bool| {
        if *has_content {
            w.line("///");
        }
        *has_content = true;
    };
    if let CallShape::Iterator(ib) = &f.shape {
        separator(w, &mut has_content);
        w.line("/// Returns a lazy [Iterable]: elements are pulled from the native");
        w.line("/// iterator one at a time (one native `next` call per element), and");
        w.line("/// iterating the result again launches a fresh native iterator.");
        w.line("///");
        w.line("/// The native iterator handle is destroyed exactly once: eagerly when");
        w.line("/// the iteration completes or fails, or by a GC finalizer if the");
        w.line("/// iteration is abandoned before it is exhausted.");
        if ib.elem.is_object_ref() {
            w.line("///");
            w.line("/// Each yielded element is owned by the caller: call its `dispose()`");
            w.line("/// when you are done with it.");
        }
    }
    if let Some(exc) = err.thrown_exception() {
        separator(w, &mut has_content);
        w.line(format!("/// Throws [{exc}] on domain errors."));
    }
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('\'', "\\'");
        w.line(format!("@Deprecated('{escaped}')"));
    }
}

/// The native FFI typedef for a module-level callback declaration, shared by
/// every listener that fires it.
fn render_callback_typedef(out: &mut String, cb: &CallbackBinding) {
    let mut slots: Vec<String> = Vec::new();
    for p in &cb.params {
        for (n, _) in input_slots(&p.ty) {
            slots.push(n);
        }
    }
    slots.push("Pointer<Void>".into());
    out.push_str(&format!(
        "\ntypedef _NativeCb_{} = Void Function({});\n",
        cb.c_fn_type,
        slots.join(", ")
    ));
}

/// The Dart expression converting one callback parameter's trampoline slots
/// into the value handed to the user callback. Slot names follow the lowered
/// ABI (`{n}`, `{n}_ptr`/`{n}_len`, `{n}_keys`/`{n}_values`/`{n}_len`).
fn cb_arg_expr(p: &ParamBinding) -> String {
    let n0 = p.abi[0].name.to_lower_camel_case();
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
        | TypeRef::Bool => n0,
        TypeRef::Enum(name) => format!(
            "{}.fromValue({n0})",
            local_type_name(name).to_upper_camel_case()
        ),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("{n0} == nullptr ? '' : {n0}.toDartString()")
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let len = p.abi[1].name.to_lower_camel_case();
            format!("{n0} == nullptr ? <int>[] : {n0}.asTypedList({len}).toList()")
        }
        // Borrowed for the duration of the callback: do not dispose().
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name)
        | TypeRef::Interface(name) => {
            format!("{}._({n0})", local_type_name(name).to_upper_camel_case())
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("{n0} == nullptr ? null : {n0}.toDartString()")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let len = p.abi[1].name.to_lower_camel_case();
                format!("{n0} == nullptr ? null : {n0}.asTypedList({len}).toList()")
            }
            TypeRef::Record(name)
            | TypeRef::RichEnum(name)
            | TypeRef::TypedHandle(name)
            | TypeRef::Interface(name) => {
                format!(
                    "{n0} == nullptr ? null : {}._({n0})",
                    local_type_name(name).to_upper_camel_case()
                )
            }
            TypeRef::Enum(name) => format!(
                "{n0} == nullptr ? null : {}.fromValue({n0}.value)",
                local_type_name(name).to_upper_camel_case()
            ),
            _ => format!("{n0} == nullptr ? null : {n0}.value"),
        },
        TypeRef::List(inner) => {
            let len = p.abi[1].name.to_lower_camel_case();
            let elem = map_elem_read(&n0, "i", inner);
            let dt = dart_type(inner);
            format!("{n0} == nullptr ? <{dt}>[] : List.generate({len}, (i) => {elem})")
        }
        TypeRef::Map(k, v) => {
            let keys = p.abi[0].name.to_lower_camel_case();
            let vals = p.abi[1].name.to_lower_camel_case();
            let len = p.abi[2].name.to_lower_camel_case();
            let kexpr = map_elem_read(&keys, "i", k);
            let vexpr = map_elem_read(&vals, "i", v);
            let (kt, vt) = (dart_type(k), dart_type(v));
            format!(
                "{keys} == nullptr ? <{kt}, {vt}>{{}} : {{ for (var i = 0; i < {len}; i++) {kexpr}: {vexpr} }}"
            )
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
}

/// The register/unregister wrapper pair for one listener. The trampoline is an
/// `isolateLocal` NativeCallable: WeaveFFI listeners fire synchronously on the
/// thread calling the producer API, so arguments are converted inside the
/// borrow window (a `.listener` callable would read freed pointers later).
fn render_listener(out: &mut String, m: &ModuleBinding, l: &ListenerBinding, strip: bool) {
    let Some(cb) = m.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let cb_typedef = format!("_NativeCb_{}", cb.c_fn_type);
    let register_name =
        wrapper_name(&m.path, &format!("register_{}", l.name), strip).to_lower_camel_case();
    let unregister_name =
        wrapper_name(&m.path, &format!("unregister_{}", l.name), strip).to_lower_camel_case();

    emit_typedef_and_lookup(
        out,
        &l.register_symbol,
        &format!("Pointer<NativeFunction<{cb_typedef}>>, Pointer<Void>"),
        &format!("Pointer<NativeFunction<{cb_typedef}>>, Pointer<Void>"),
        "Uint64",
        "int",
    );
    emit_typedef_and_lookup(out, &l.unregister_symbol, "Uint64", "int", "Void", "void");

    let user_fn_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), p.name.to_lower_camel_case()))
        .collect();
    let mut tramp_decls: Vec<String> = Vec::new();
    for p in &cb.params {
        for ((_, d), slot) in input_slots(&p.ty).iter().zip(p.abi.iter()) {
            tramp_decls.push(format!("{d} {}", slot.name.to_lower_camel_case()));
        }
    }
    tramp_decls.push("Pointer<Void> context".into());
    let call_args: Vec<String> = cb.params.iter().map(cb_arg_expr).collect();

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &l.doc, "");
        w.raw(d);
    }
    w.line(format!(
        "/// Registers a {} listener. Returns a subscription id for {unregister_name}().",
        cb.name
    ));
    w.block(
        format!(
            "int {register_name}(void Function({}) callback) {{",
            user_fn_params.join(", ")
        ),
        "}",
        |w| {
            w.line(format!(
                "final callable = NativeCallable<{cb_typedef}>.isolateLocal(({}) {{",
                tramp_decls.join(", ")
            ));
            w.scope(|w| {
                w.line(format!("callback({});", call_args.join(", ")));
            });
            w.line("});");
            w.line(format!(
                "final id = _{}(callable.nativeFunction, nullptr);",
                l.register_symbol.to_lower_camel_case()
            ));
            w.line("_listenerCallables[id] = callable;");
            w.line("return id;");
        },
    );

    w.blank();
    w.line(format!(
        "/// Unregisters a listener previously registered with {register_name}()."
    ));
    w.block(format!("void {unregister_name}(int id) {{"), "}", |w| {
        w.line(format!(
            "_{}(id);",
            l.unregister_symbol.to_lower_camel_case()
        ));
        w.line("_listenerCallables.remove(id)?.close();");
    });
    out.push_str(&w.finish());
}

/// Returns the (native, dart) FFI types of the trailing callback parameters
/// (those after `(context, err)`) for an async function with the given return
/// type. The empty vec means the callback signature is `(context, err)` with
/// no extra payload. Buffer results arrive as borrowed `(ptr, len)` pairs
/// typed like the sync array shapes; a map arrives as parallel key/value
/// arrays.
fn async_cb_extra_params(ret: Option<&TypeRef>) -> Vec<(String, String)> {
    let ptr = |s: String| (s.clone(), s);
    match ret {
        None => vec![],
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            vec![ptr("Pointer<Uint8>".into()), ("Size".into(), "int".into())]
        }
        Some(TypeRef::List(inner)) => vec![
            ptr(input_array_ffi(inner)),
            ("Size".into(), "int".into()),
        ],
        Some(TypeRef::Map(k, v)) => vec![
            ptr(input_array_ffi(k)),
            ptr(input_array_ffi(v)),
            ("Size".into(), "int".into()),
        ],
        Some(TypeRef::Optional(inner)) => vec![match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => ptr("Pointer<Utf8>".into()),
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => ptr("Pointer<Void>".into()),
            // A boxed optional scalar arrives as a nullable pointer-to-scalar.
            other => ptr(format!("Pointer<{}>", scalar_ffi(other).0)),
        }],
        Some(t) => vec![match t {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => ptr("Pointer<Utf8>".into()),
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => ptr("Pointer<Void>".into()),
            other => {
                let (n, d) = scalar_ffi(other);
                (n.into(), d.into())
            }
        }],
    }
}

/// The Dart parameter names of an async callback's trailing result slots,
/// mirroring [`async_cb_extra_params`].
fn async_cb_arg_names(ret: Option<&TypeRef>) -> Vec<String> {
    let mut names = vec!["context".to_string(), "err".to_string()];
    match ret {
        None => {}
        Some(TypeRef::Map(_, _)) => {
            names.extend(["resultKeys".into(), "resultValues".into(), "resultLen".into()]);
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_)) => {
            names.extend(["result".into(), "resultLen".into()]);
        }
        Some(_) => names.push("result".into()),
    }
    names
}

/// Render one async callable: its callback typedef and launcher lookup into
/// `lookups`, and its `Future`-returning wrapper into `decl`. A method's
/// launcher carries the implicit leading `self` pointer.
#[allow(clippy::too_many_arguments)]
fn render_async_function(
    lookups: &mut String,
    decl: &mut String,
    c_sym: &str,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
    pub_ret: &str,
    wrapper_params: &[String],
    err: ErrCtx,
) {
    let cb_extras = async_cb_extra_params(f.ret.as_ref());
    let cb_native_params: Vec<String> = std::iter::once("Pointer<Void>".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError>".to_string()))
        .chain(cb_extras.iter().map(|(n, _)| n.clone()))
        .collect();

    let cb_typedef = format!("_NativeAsyncCb_{c_sym}");
    lookups.push_str(&format!(
        "\ntypedef {cb_typedef} = Void Function({});\n",
        cb_native_params.join(", ")
    ));

    let async_sym = format!("{c_sym}_async");
    let self_slot = if f.has_self {
        vec!["Pointer<Void>".to_string()]
    } else {
        vec![]
    };
    let mut native_params: Vec<String> = self_slot.clone();
    native_params.extend(f.params.iter().map(|p| native_ffi_type(&p.ty)));
    if f.cancellable {
        native_params.push("Pointer<Void>".into());
    }
    native_params.push(format!("Pointer<NativeFunction<{cb_typedef}>>"));
    native_params.push("Pointer<Void>".into());
    let mut dart_params: Vec<String> = self_slot;
    dart_params.extend(f.params.iter().map(|p| dart_ffi_type(&p.ty)));
    if f.cancellable {
        dart_params.push("Pointer<Void>".into());
    }
    dart_params.push(format!("Pointer<NativeFunction<{cb_typedef}>>"));
    dart_params.push("Pointer<Void>".into());

    emit_typedef_and_lookup(
        lookups,
        &async_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        "Void",
        "void",
    );

    let completer_type = if f.ret.is_some() {
        pub_ret.to_string()
    } else {
        "void".to_string()
    };

    // String inputs are pinned across the async call and freed on completion;
    // capture `(pointer name, source name)` up front so the writer emits the
    // staging in order and the cleanup can reference the pointers.
    let native_strings: Vec<(String, String)> = f
        .params
        .iter()
        .filter(|p| matches!(p.ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr))
        .map(|p| {
            let pname = p.name.to_lower_camel_case();
            (format!("{pname}Ptr"), pname)
        })
        .collect();

    let cb_dart_params: Vec<String> = std::iter::once("Pointer<Void>".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError>".to_string()))
        .chain(cb_extras.iter().map(|(_, d)| d.clone()))
        .collect();
    let cb_arg_names = async_cb_arg_names(f.ret.as_ref());
    let cb_param_decls: Vec<String> = cb_dart_params
        .iter()
        .zip(cb_arg_names.iter())
        .map(|(t, n)| format!("{t} {n}"))
        .collect();

    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    for p in &f.params {
        let pname = p.name.to_lower_camel_case();
        call_args.push(match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{pname}Ptr"),
            TypeRef::Enum(_) => format!("{pname}.value"),
            TypeRef::TypedHandle(_)
            | TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_) => {
                format!("{pname}._handle")
            }
            _ => pname,
        });
    }
    if f.cancellable {
        call_args.push("nullptr".into());
    }
    call_args.push("callable.nativeFunction".into());
    call_args.push("nullptr".into());

    let var = async_sym.to_lower_camel_case();

    let mut ac = String::new();
    emit_async_complete(&mut ac, f.ret.as_ref(), "      ");

    let mut w = CodeWriter::two_space();
    w.blank();
    emit_wrapper_doc(&mut w, f, err);
    w.block(
        kind.open_line(
            &format!("Future<{pub_ret}>"),
            name,
            &wrapper_params.join(", "),
        ),
        "}",
        |w| {
            w.line(format!("final completer = Completer<{completer_type}>();"));
            for (ptr, pname) in &native_strings {
                w.line(format!("final {ptr} = {pname}.toNativeUtf8();"));
            }
            w.line(format!("late NativeCallable<{cb_typedef}> callable;"));
            w.line(format!(
                "callable = NativeCallable<{cb_typedef}>.listener(({}) {{",
                cb_param_decls.join(", ")
            ));
            w.scope(|w| {
                w.line("try {");
                w.scope(|w| {
                    w.line("if (err.address != 0 && err.ref.code != 0) {");
                    w.scope(|w| {
                        w.line("final code = err.ref.code;");
                        w.line("final msg = err.ref.message.toDartString();");
                        w.line("_weaveffiErrorClear(err);");
                        w.line(format!("completer.completeError({});", err.map_expr()));
                        w.line("return;");
                    });
                    w.line("}");
                    w.raw(&ac);
                });
                w.line("} catch (e) {");
                w.scope(|w| {
                    w.line("completer.completeError(e);");
                });
                w.line("} finally {");
                w.scope(|w| {
                    w.line("callable.close();");
                });
                w.line("}");
            });
            w.line("});");
            w.line("try {");
            w.scope(|w| {
                w.line(format!("_{var}({});", call_args.join(", ")));
            });
            w.line("} catch (e) {");
            w.scope(|w| {
                w.line("callable.close();");
                for (ptr, _) in &native_strings {
                    w.line(format!("calloc.free({ptr});"));
                }
                w.line("rethrow;");
            });
            w.line("}");
            if native_strings.is_empty() {
                w.line("return completer.future;");
            } else {
                w.line("return completer.future.whenComplete(() {");
                w.scope(|w| {
                    for (ptr, _) in &native_strings {
                        w.line(format!("calloc.free({ptr});"));
                    }
                });
                w.line("});");
            }
        },
    );
    decl.push_str(&w.finish());
}

/// Emit the callback statements that resolve the completer from the result
/// slots. Borrowed buffers (strings, bytes, arrays, map buffers, boxed
/// optional scalars) are valid only for the callback's duration, so they are
/// deep-copied here and never freed; an owned-object result (record, rich
/// enum, interface) is instead adopted by its wrapper class.
fn emit_async_complete(out: &mut String, ty: Option<&TypeRef>, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match ty {
        None => {
            w.line("completer.complete();");
        }
        Some(TypeRef::Enum(name)) => {
            let n = local_type_name(name).to_upper_camel_case();
            w.line(format!("completer.complete({n}.fromValue(result));"));
        }
        // The callback receives ownership of an object result; the wrapper
        // adopts the pointer and its `dispose()` owns the eventual destroy.
        Some(
            TypeRef::Record(name)
            | TypeRef::RichEnum(name)
            | TypeRef::TypedHandle(name)
            | TypeRef::Interface(name),
        ) => {
            let n = local_type_name(name).to_upper_camel_case();
            w.line(format!("completer.complete({n}._(result));"));
        }
        // Borrowed: copy before the callback returns, never free.
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            w.line("completer.complete(result.toDartString());");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            w.line("completer.complete(result == nullptr");
            w.line("    ? <int>[]");
            w.line("    : List<int>.generate(resultLen, (i) => result[i]));");
        }
        Some(TypeRef::List(inner)) => {
            let dt = dart_type(inner);
            let read = map_elem_read("result", "i", inner);
            w.line("completer.complete(result == nullptr");
            w.line(format!("    ? <{dt}>[]"));
            w.line(format!(
                "    : List<{dt}>.generate(resultLen, (i) => {read}));"
            ));
        }
        Some(TypeRef::Map(k, v)) => {
            let (kt, vt) = (dart_type(k), dart_type(v));
            let kread = map_elem_read("resultKeys", "i", k);
            let vread = map_elem_read("resultValues", "i", v);
            w.line(format!("final map = <{kt}, {vt}>{{}};"));
            w.line("if (resultKeys != nullptr) {");
            w.scope(|w| {
                w.line("for (var i = 0; i < resultLen; i++) {");
                w.scope(|w| {
                    w.line(format!("map[{kread}] = {vread};"));
                });
                w.line("}");
            });
            w.line("}");
            w.line("completer.complete(map);");
        }
        Some(TypeRef::Optional(inner)) => {
            w.line("if (result == nullptr) {");
            w.scope(|w| {
                w.line("completer.complete(null);");
            });
            w.line("} else {");
            w.scope(|w| match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line("completer.complete(result.toDartString());");
                }
                TypeRef::Record(name)
                | TypeRef::RichEnum(name)
                | TypeRef::TypedHandle(name)
                | TypeRef::Interface(name) => {
                    let n = local_type_name(name).to_upper_camel_case();
                    w.line(format!("completer.complete({n}._(result));"));
                }
                TypeRef::Enum(name) => {
                    let n = local_type_name(name).to_upper_camel_case();
                    w.line(format!("completer.complete({n}.fromValue(result.value));"));
                }
                // Boxed optional scalar: copy the value, never free the box.
                _ => {
                    w.line("completer.complete(result.value);");
                }
            });
            w.line("}");
        }
        Some(_) => {
            w.line("completer.complete(result);");
        }
    }
    out.push_str(&w.finish());
}

fn emit_function_body(out: &mut String, f: &FnBinding, c_sym: &str, err: ErrCtx) {
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut staging = String::new();
    for p in &f.params {
        let args = emit_input(
            &mut staging,
            &p.name.to_lower_camel_case(),
            &p.ty,
            &mut frees,
        );
        call_args.extend(args);
    }
    if let Some(ret) = &f.ret {
        call_args.extend(emit_return_alloc(&mut staging, ret, &mut frees, "  "));
    }
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    let args = call_args.join(", ");
    // A map return is a `void` symbol whose results land in the out-params.
    let void_call = f.ret.is_none() || matches!(&f.ret, Some(TypeRef::Map(_, _)));
    let mut dec = String::new();
    if let Some(ret) = &f.ret {
        emit_return_decode(&mut dec, ret, "    ");
    }

    let mut w = CodeWriter::two_space().with_depth(1);
    w.raw(staging);
    w.line("final err = calloc<_WeaveFFIError>();");
    w.line("try {");
    w.scope(|w| {
        if void_call {
            w.line(format!("_{var}({args});"));
        } else {
            w.line(format!("final result = _{var}({args});"));
        }
        w.line(err.check_stmt());
        w.raw(&dec);
    });
    w.line("} finally {");
    w.scope(|w| {
        for fr in &frees {
            w.line(fr);
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Bind the element `next`/`destroy` symbols of an iterator-returning
/// function, plus a `NativeFinalizer` over the destroy symbol. The finalizer
/// is the disposal backstop for abandoned iterations: Dart runs a `sync*`
/// body only inside `moveNext`, so a consumer that stops pulling (a broken
/// `for` loop, `first`, `take`) never resumes the generator and its `finally`
/// block never runs; the finalizer reclaims the native handle when the
/// suspended frame is collected instead.
fn emit_iter_lookups(out: &mut String, ib: &IteratorBinding) {
    let item = input_array_ffi(&ib.elem);
    emit_typedef_and_lookup(
        out,
        &ib.next.symbol,
        &format!("Pointer<Void>, {item}, Pointer<_WeaveFFIError>"),
        &format!("Pointer<Void>, {item}, Pointer<_WeaveFFIError>"),
        "Int32",
        "int",
    );
    emit_typedef_and_lookup(
        out,
        &ib.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
    out.push_str(&format!(
        "final _{}Finalizer = NativeFinalizer(\n    \
         _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('{}'));\n",
        ib.destroy_symbol.to_lower_camel_case(),
        ib.destroy_symbol
    ));
}

/// Emit the `sync*` generator body of an `iter<T>` wrapper.
///
/// The body runs lazily, on the first pull: it stages the inputs, launches
/// the C iterator, and then issues exactly one producer `next` call per
/// yielded element, releasing each element per the plan's [`ElemFree`] after
/// copying (strings through `weaveffi_free_string`; object elements are
/// adopted by their wrapper class, whose `dispose()` owns the destroy).
///
/// The handle is destroyed exactly once. The `try`/`finally` destroys it when
/// iteration exhausts, a launch or `next` error throws, or the generator is
/// otherwise torn down, then nulls the local handle so the finalizer detach
/// path cannot double-destroy. For iterations abandoned mid-stream (where the
/// `finally` never runs, see [`emit_iter_lookups`]) the `NativeFinalizer`
/// attached to the generator-local anchor destroys the handle when the frame
/// is collected; the eager path detaches before destroying.
fn emit_iterator_body(
    out: &mut String,
    f: &FnBinding,
    c_sym: &str,
    ib: &IteratorBinding,
    err: ErrCtx,
    module: &ModuleBinding,
    prefix: &str,
) {
    let proto = ib.protocol(f, &module.path, prefix);
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut staging = String::new();
    for p in &f.params {
        let args = emit_input(
            &mut staging,
            &p.name.to_lower_camel_case(),
            &p.ty,
            &mut frees,
        );
        call_args.extend(args);
    }
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    let elem = &ib.elem;
    let next_var = ib.next.symbol.to_lower_camel_case();
    let destroy_var = ib.destroy_symbol.to_lower_camel_case();

    let mut w = CodeWriter::two_space().with_depth(1);
    w.raw(staging);
    w.line("final err = calloc<_WeaveFFIError>();");
    w.line(format!("final outItem = calloc<{}>();", elem_pointee(elem)));
    w.line("Pointer<Void> iter = nullptr;");
    w.line("final anchor = _IteratorLifetime();");
    w.line("try {");
    w.scope(|w| {
        w.line(format!("iter = _{var}({});", call_args.join(", ")));
        w.line(err.check_stmt());
        w.line(format!(
            "_{destroy_var}Finalizer.attach(anchor, iter, detach: anchor);"
        ));
        w.line(format!("while (_{next_var}(iter, outItem, err) != 0) {{"));
        w.scope(|w| {
            w.line(err.check_stmt());
            match &proto.elem_free {
                ElemFree::String => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final item = itemPtr.toDartString();");
                    w.line("_weaveffiFreeString(itemPtr);");
                    w.line("yield item;");
                }
                // The consumer adopts an object element; its wrapper's
                // dispose() owns the eventual destroy.
                ElemFree::Object { .. } | ElemFree::None => {
                    w.line(format!("yield {};", read_value("outItem.value", elem)));
                }
            }
        });
        w.line("}");
        w.line(err.check_stmt());
    });
    w.line("} finally {");
    w.scope(|w| {
        w.line("if (iter != nullptr) {");
        w.scope(|w| {
            w.line(format!("_{destroy_var}Finalizer.detach(anchor);"));
            w.line(format!("_{destroy_var}(iter);"));
            w.line("iter = nullptr;");
        });
        w.line("}");
        w.line("calloc.free(outItem);");
        for fr in &frees {
            w.line(fr);
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Materialises a `T*` + `out_len` C return into a Dart `List<T>`, then
/// releases what the wrapper owes per the plan: string elements through
/// `weaveffi_free_string` after copying, and the producer-allocated array
/// buffer itself through `weaveffi_free_bytes` (`n * sizeOf<elem>`). Object
/// elements transfer ownership to the caller, who disposes each wrapper.
fn emit_list_conversion(out: &mut String, inner: &TypeRef, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line("final n = outLen.value;");
    let dt = dart_type(inner);
    w.line(format!("if (result == nullptr) return <{dt}>[];"));
    let pointee = elem_pointee(inner);
    w.line(format!("final arr = result.cast<{pointee}>();"));
    w.line(format!(
        "final items = List<{dt}>.generate(n, (i) => {});",
        map_elem_read("arr", "i", inner)
    ));
    if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        w.line("for (var i = 0; i < n; i++) {");
        w.scope(|w| {
            w.line("_weaveffiFreeString(arr[i]);");
        });
        w.line("}");
    }
    w.line(format!(
        "_weaveffiFreeBytes(result.cast(), n * sizeOf<{pointee}>());"
    ));
    w.line("return items;");
    out.push_str(&w.finish());
}

/// Emit the post-call conversion of a simple (non-out-param) return, paying
/// the release the wrapper owes per the plan: a returned string is copied and
/// then freed with `weaveffi_free_string`, a boxed optional scalar is
/// dereferenced and freed with `weaveffi_free_bytes`, and an owned object
/// pointer is adopted by its wrapper class (whose `dispose()` owns the
/// destroy).
fn emit_result_conversion(out: &mut String, ty: &TypeRef, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("final value = result.toDartString();");
            w.line("_weaveffiFreeString(result);");
            w.line("return value;");
        }
        TypeRef::Enum(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            w.line(format!("return {n}.fromValue(result);"));
        }
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::TypedHandle(name)
        | TypeRef::Interface(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            w.line(format!("return {n}._(result);"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line("if (result == nullptr) return null;");
                w.line("final value = result.toDartString();");
                w.line("_weaveffiFreeString(result);");
                w.line("return value;");
            }
            TypeRef::Record(name)
            | TypeRef::RichEnum(name)
            | TypeRef::TypedHandle(name)
            | TypeRef::Interface(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                w.line("if (result == nullptr) return null;");
                w.line(format!("return {n}._(result);"));
            }
            // Optional scalars/bools/enums lower to a producer-boxed nullable
            // pointer-to-scalar: dereference, then free the box.
            TypeRef::Enum(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                w.line("if (result == nullptr) return null;");
                w.line(format!("final value = {n}.fromValue(result.value);"));
                w.line("_weaveffiFreeBytes(result.cast(), sizeOf<Int32>());");
                w.line("return value;");
            }
            other => {
                let native = scalar_ffi(other).0;
                w.line("if (result == nullptr) return null;");
                w.line("final value = result.value;");
                w.line(format!(
                    "_weaveffiFreeBytes(result.cast(), sizeOf<{native}>());"
                ));
                w.line("return value;");
            }
        },
        _ => {
            w.line("return result;");
        }
    }
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;

    #[test]
    fn package_bundles_native_and_rewrites_loader() {
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![simple_module(vec![Function {
            name: "ping".into(),
            params: vec![],
            returns: None,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::LinuxArm64, "/s/linux-arm64/libcalculator.so");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &DartGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &DartConfig::default(),
        )
        .expect("dart supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("dart/native/linux-arm64/libcalculator.so")));
        let module = files
            .iter()
            .find(|f| f.path.as_str().ends_with("dart/lib/weaveffi.dart"))
            .expect("module present");
        let FileContent::Text(src) = &module.content else {
            panic!("module is text");
        };
        assert!(
            src.contains("final candidates = <String>[]")
                && src.contains("native/darwin-arm64/libcalculator.dylib"),
            "packaged loader not applied: {src}"
        );
    }
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn simple_module(functions: Vec<Function>) -> Module {
        Module {
            name: "math".into(),
            functions,
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }
    }

    /// Build the binding model and render the module exactly as the driver
    /// does in production before calling [`LanguageBackend::files`]. Shadows
    /// the production three-argument renderer for the test suite.
    fn render_dart_module(api: &Api, prefix: &str, input_basename: &str) -> String {
        let model = BindingModel::build(api, prefix);
        let config = DartConfig {
            prefix: Some(prefix.to_string()),
            input_basename: Some(input_basename.to_string()),
            ..DartConfig::default()
        };
        super::render_dart_module(api, &model, &config)
    }

    #[test]
    fn generator_name_is_dart() {
        assert_eq!(Generator::name(&DartGenerator), "dart");
    }

    #[test]
    fn output_files_lists_dart_file() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = DartGenerator.output_files(&api, out, &DartConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out}/dart/README.md"),
                format!("{out}/dart/lib/weaveffi.dart"),
                format!("{out}/dart/pubspec.yaml"),
            ]
        );
    }

    #[test]
    fn dart_type_mapping() {
        assert_eq!(dart_type(&TypeRef::I32), "int");
        assert_eq!(dart_type(&TypeRef::U32), "int");
        assert_eq!(dart_type(&TypeRef::I64), "int");
        assert_eq!(dart_type(&TypeRef::F64), "double");
        assert_eq!(dart_type(&TypeRef::Bool), "bool");
        assert_eq!(dart_type(&TypeRef::StringUtf8), "String");
        assert_eq!(dart_type(&TypeRef::Handle), "int");
        assert_eq!(dart_type(&TypeRef::Record("Foo".into())), "Foo");
        assert_eq!(dart_type(&TypeRef::RichEnum("Shape".into())), "Shape");
        assert_eq!(dart_type(&TypeRef::Enum("Bar".into())), "Bar");
        assert_eq!(
            dart_type(&TypeRef::TypedHandle("Session".into())),
            "Session"
        );
        assert_eq!(
            dart_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "int?"
        );
        assert_eq!(
            dart_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "List<int>"
        );
        assert_eq!(
            dart_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Map<String, int>"
        );
    }

    #[test]
    fn native_ffi_type_mapping() {
        assert_eq!(native_ffi_type(&TypeRef::I32), "Int32");
        assert_eq!(native_ffi_type(&TypeRef::U32), "Uint32");
        assert_eq!(native_ffi_type(&TypeRef::I64), "Int64");
        assert_eq!(native_ffi_type(&TypeRef::F64), "Double");
        // A C `bool` is one byte; `Bool` keeps strides and frees honest.
        assert_eq!(native_ffi_type(&TypeRef::Bool), "Bool");
        assert_eq!(native_ffi_type(&TypeRef::StringUtf8), "Pointer<Utf8>");
        assert_eq!(native_ffi_type(&TypeRef::Handle), "Int64");
        assert_eq!(
            native_ffi_type(&TypeRef::Record("X".into())),
            "Pointer<Void>"
        );
        assert_eq!(
            native_ffi_type(&TypeRef::RichEnum("X".into())),
            "Pointer<Void>"
        );
        assert_eq!(native_ffi_type(&TypeRef::Enum("X".into())), "Int32");
        assert_eq!(
            native_ffi_type(&TypeRef::TypedHandle("S".into())),
            "Pointer<Void>"
        );
    }

    #[test]
    fn generate_dart_basic() {
        let api = make_api(vec![simple_module(vec![Function {
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
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dart_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator
            .generate(&api, out_dir, &DartConfig::default())
            .unwrap();

        let dart = std::fs::read_to_string(tmp.join("dart/lib/weaveffi.dart")).unwrap();

        assert!(
            dart.contains("import 'dart:ffi'"),
            "missing dart:ffi import: {dart}"
        );
        assert!(
            dart.contains("import 'package:ffi/ffi.dart'"),
            "missing ffi package import: {dart}"
        );
        assert!(
            dart.contains("import 'dart:io' show Platform"),
            "missing Platform import: {dart}"
        );
        assert!(
            dart.contains("DynamicLibrary _openLibrary()"),
            "missing _openLibrary: {dart}"
        );
        assert!(
            dart.contains("libweaveffi.dylib"),
            "missing macOS lib: {dart}"
        );
        assert!(dart.contains("libweaveffi.so"), "missing Linux lib: {dart}");
        assert!(dart.contains("weaveffi.dll"), "missing Windows lib: {dart}");
        assert!(
            dart.contains("final DynamicLibrary _lib"),
            "missing _lib: {dart}"
        );
        assert!(
            dart.contains("_WeaveFFIError extends Struct"),
            "missing error struct: {dart}"
        );
        assert!(
            dart.contains("class WeaveFFIException"),
            "missing exception class: {dart}"
        );
        assert!(dart.contains("_checkError"), "missing error check: {dart}");
        assert!(
            dart.contains("weaveffi_error_clear"),
            "missing error_clear: {dart}"
        );
        assert!(
            dart.contains("typedef _NativeWeaveffiMathAdd"),
            "missing native typedef: {dart}"
        );
        assert!(
            dart.contains("typedef _DartWeaveffiMathAdd"),
            "missing dart typedef: {dart}"
        );
        assert!(
            dart.contains("lookupFunction"),
            "missing lookupFunction: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_math_add'"),
            "missing C symbol: {dart}"
        );
        assert!(
            dart.contains("Int32 Function(Int32, Int32"),
            "missing native sig: {dart}"
        );
        assert!(
            dart.contains("int Function(int, int"),
            "missing dart sig: {dart}"
        );
        assert!(
            dart.contains("int add(int a, int b)"),
            "missing wrapper: {dart}"
        );
        assert!(
            dart.contains("calloc<_WeaveFFIError>()"),
            "missing calloc: {dart}"
        );
        assert!(
            dart.contains("_checkError(err)"),
            "missing error check in wrapper: {dart}"
        );
        assert!(dart.contains("return result"), "missing return: {dart}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dart_with_structs() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
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
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

        assert!(dart.contains("class Contact {"), "missing class: {dart}");
        assert!(
            dart.contains("Pointer<Void> _handle"),
            "missing _handle: {dart}"
        );
        assert!(
            dart.contains("Contact._(this._handle)"),
            "missing constructor: {dart}"
        );
        assert!(dart.contains("void dispose()"), "missing dispose: {dart}");
        assert!(
            dart.contains("weaveffi_contacts_Contact_destroy"),
            "missing destroy sym: {dart}"
        );
        assert!(dart.contains("int get id"), "missing id getter: {dart}");
        assert!(
            dart.contains("weaveffi_contacts_Contact_get_id"),
            "missing id getter sym: {dart}"
        );
        assert!(
            dart.contains("String get firstName"),
            "missing firstName getter: {dart}"
        );
        assert!(
            dart.contains("weaveffi_contacts_Contact_get_first_name"),
            "missing firstName getter sym: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString: {dart}"
        );
        assert!(
            dart.contains("String? get email"),
            "missing email getter: {dart}"
        );
        assert!(
            dart.contains("weaveffi_contacts_Contact_get_email"),
            "missing email getter sym: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_builder_struct() {
        let api = make_api(vec![Module {
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("class PointBuilder {"),
            "builder class: {dart}"
        );
        assert!(
            dart.contains("PointBuilder withX(double value)"),
            "fluent setter: {dart}"
        );
        assert!(dart.contains("Point build() {"), "build method: {dart}");
        assert!(
            dart.contains("_checkError(err);") && dart.contains("return Point._(result);"),
            "build calls FFI create and wraps the result: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_enums() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![Function {
                name: "mix".into(),
                params: vec![Param {
                    name: "color".into(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
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
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

        assert!(dart.contains("enum Color {"), "missing enum: {dart}");
        assert!(dart.contains("red(0)"), "missing red: {dart}");
        assert!(dart.contains("green(1)"), "missing green: {dart}");
        assert!(dart.contains("blue(2)"), "missing blue: {dart}");
        assert!(
            dart.contains("const Color(this.value)"),
            "missing const constructor: {dart}"
        );
        assert!(
            dart.contains("final int value"),
            "missing value field: {dart}"
        );
        assert!(
            dart.contains("static Color fromValue(int value)"),
            "missing fromValue: {dart}"
        );
        assert!(
            dart.contains("Color mix(Color color)"),
            "missing mix signature: {dart}"
        );
        assert!(
            dart.contains("color.value"),
            "missing .value conversion: {dart}"
        );
        assert!(
            dart.contains("Color.fromValue(result)"),
            "missing fromValue conversion: {dart}"
        );
    }

    #[test]
    fn void_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "reset".into(),
            params: vec![],
            returns: None,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("void reset()"),
            "missing void function: {dart}"
        );
        assert!(
            dart.contains("Void Function("),
            "missing Void native return: {dart}"
        );
    }

    #[test]
    fn string_function() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("String echo(String msg)"),
            "missing signature: {dart}"
        );
        assert!(
            dart.contains("toNativeUtf8()"),
            "missing toNativeUtf8: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString: {dart}"
        );
        assert!(
            dart.contains("calloc.free(msgPtr)"),
            "missing free for string: {dart}"
        );
        // The returned `const char*` is owned by the caller: copy first,
        // then release it through the runtime.
        assert!(
            dart.contains("final value = result.toDartString();\n    _weaveffiFreeString(result);"),
            "returned string must be copied then freed: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_free_string'"),
            "missing weaveffi_free_string lookup: {dart}"
        );
    }

    #[test]
    fn bool_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "is_valid".into(),
            params: vec![Param {
                name: "flag".into(),
                ty: TypeRef::Bool,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("bool isValid(bool flag)"),
            "missing signature: {dart}"
        );
        // A C `bool` crosses as the one-byte dart:ffi `Bool`, so the wrapper
        // passes and returns Dart bools without integer conversions.
        assert!(
            dart.contains("Bool Function(Bool, Pointer<_WeaveFFIError>)"),
            "missing Bool native signature: {dart}"
        );
        assert!(
            dart.contains("bool Function(bool, Pointer<_WeaveFFIError>)"),
            "missing bool dart signature: {dart}"
        );
        assert!(
            !dart.contains("flag ? 1 : 0") && !dart.contains("result != 0;"),
            "bool must not round-trip through ints: {dart}"
        );
    }

    #[test]
    fn async_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("import 'dart:async'"),
            "missing dart:async import: {dart}"
        );
        assert!(
            dart.contains("Future<String> fetchData(int id)"),
            "missing async wrapper: {dart}"
        );
        assert!(
            dart.contains("NativeCallable<_NativeAsyncCb_weaveffi_math_fetch_data>.listener"),
            "missing NativeCallable.listener: {dart}"
        );
        assert!(
            dart.contains("weaveffi_math_fetch_data_async"),
            "must call the _async C symbol: {dart}"
        );
    }

    /// `NativeCallable.listener` allocates a native trampoline that pins the
    /// Dart closure across the C boundary. It must be matched by exactly one
    /// `callable.close()` on every exit path so the trampoline is freed when
    /// the future resolves.
    #[test]
    fn dart_async_pins_callback_for_lifetime() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        let pin_count = dart.matches(".listener(").count();
        let unpin_count = dart.matches("callable.close()").count();
        assert_eq!(
            pin_count, 1,
            "expected one NativeCallable.listener per async fn, got {pin_count}: {dart}"
        );
        // Two close sites per fn: callback finally, and try/catch around _ffiCall.
        assert_eq!(
            unpin_count, 2,
            "expected callable.close() in callback finally and synchronous catch (2 total), got {unpin_count}: {dart}"
        );
    }

    #[test]
    fn struct_return_wraps_handle() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Contact getContact(int id)"),
            "missing signature: {dart}"
        );
        assert!(
            dart.contains("Contact._(result)"),
            "missing struct wrapping: {dart}"
        );
    }

    #[test]
    fn handle_uses_int64() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "create".into(),
            params: vec![],
            returns: Some(TypeRef::Handle),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Int64 Function("),
            "missing Int64 for Handle: {dart}"
        );
    }

    #[test]
    fn dart_generates_pubspec() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_pubspec");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator
            .generate(&api, out_dir, &DartConfig::default())
            .unwrap();

        let pubspec_path = tmp.join("dart/pubspec.yaml");
        assert!(pubspec_path.exists(), "pubspec.yaml should exist");
        let pubspec = std::fs::read_to_string(&pubspec_path).unwrap();
        assert!(
            pubspec.contains("name: weaveffi"),
            "missing name: {pubspec}"
        );
        assert!(
            pubspec.contains("version: 0.1.0"),
            "missing version: {pubspec}"
        );
        assert!(
            pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
            "missing sdk constraint: {pubspec}"
        );
        assert!(
            pubspec.contains("ffi: ^2.0.0"),
            "missing ffi dependency: {pubspec}"
        );

        let readme_path = tmp.join("dart/README.md");
        assert!(readme_path.exists(), "README.md should exist");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        assert!(
            readme.contains("dart:ffi"),
            "README should mention dart:ffi: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dart_with_optionals() {
        let api = make_api(vec![Module {
            name: "users".into(),
            functions: vec![Function {
                name: "find_user".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("String? findUser(int id)"),
            "missing optional return type: {dart}"
        );
        assert!(
            dart.contains("if (result == nullptr) return null;"),
            "missing null check: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString for optional: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_lists() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![Function {
                name: "get_scores".into(),
                params: vec![Param {
                    name: "items".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("List<String> getScores(List<int> items)"),
            "missing list signature: {dart}"
        );
        assert!(
            dart.contains("Pointer<Void>"),
            "missing Pointer<Void> for list FFI type: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_maps() {
        let api = make_api(vec![Module {
            name: "cache".into(),
            functions: vec![Function {
                name: "get_entries".into(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Map<String, int> getEntries()"),
            "missing map return type: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_typed_handle() {
        let api = make_api(vec![Module {
            name: "sessions".into(),
            functions: vec![
                Function {
                    name: "create_session".into(),
                    params: vec![Param {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::TypedHandle("Session".into())),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "close_session".into(),
                    params: vec![Param {
                        name: "session".into(),
                        ty: TypeRef::TypedHandle("Session".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    throws: false,
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Session createSession(String name)"),
            "missing typed handle return: {dart}"
        );
        assert!(
            dart.contains("Session._(result)"),
            "missing typed handle wrapping: {dart}"
        );
        assert!(
            dart.contains("void closeSession(Session session)"),
            "missing typed handle param: {dart}"
        );
        assert!(
            dart.contains("session._handle"),
            "missing _handle access for typed handle param: {dart}"
        );
    }

    #[test]
    fn generate_dart_full_contacts() {
        let api = make_api(vec![Module {
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
                    throws: false,
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
                    returns: Some(TypeRef::Record("Contact".into())),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                    doc: None,
                    throws: false,
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
                    throws: false,
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
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

        assert!(
            dart.contains("enum ContactType {"),
            "missing ContactType enum: {dart}"
        );
        assert!(
            dart.contains("personal(0)"),
            "missing personal variant: {dart}"
        );
        assert!(dart.contains("work(1)"), "missing work variant: {dart}");
        assert!(dart.contains("other(2)"), "missing other variant: {dart}");

        assert!(
            dart.contains("class Contact {"),
            "missing Contact class: {dart}"
        );
        assert!(
            dart.contains("/// A contact record"),
            "missing doc comment: {dart}"
        );
        assert!(dart.contains("int get id"), "missing id getter: {dart}");
        assert!(
            dart.contains("String get firstName"),
            "missing firstName getter: {dart}"
        );
        assert!(
            dart.contains("String get lastName"),
            "missing lastName getter: {dart}"
        );
        assert!(
            dart.contains("String? get email"),
            "missing optional email getter: {dart}"
        );
        assert!(
            dart.contains("ContactType get contactType"),
            "missing contactType getter: {dart}"
        );

        assert!(
            dart.contains("int createContact("),
            "missing createContact: {dart}"
        );
        assert!(
            dart.contains("Contact getContact(int id)"),
            "missing getContact: {dart}"
        );
        assert!(
            dart.contains("List<Contact> listContacts()"),
            "missing listContacts: {dart}"
        );
        assert!(
            dart.contains("bool deleteContact(int id)"),
            "missing deleteContact: {dart}"
        );
        assert!(
            dart.contains("int countContacts()"),
            "missing countContacts: {dart}"
        );
    }

    #[test]
    fn dart_custom_package_name() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_custom_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        let config = DartConfig {
            package_name: Some("my_custom_dart".into()),
            ..DartConfig::default()
        };
        DartGenerator.generate(&api, out_dir, &config).unwrap();

        let pubspec = std::fs::read_to_string(tmp.join("dart/pubspec.yaml")).unwrap();
        assert!(
            pubspec.contains("name: my_custom_dart"),
            "pubspec should use custom package name: {pubspec}"
        );
        assert!(
            !pubspec.contains("name: weaveffi"),
            "pubspec should not use default name: {pubspec}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_no_double_free_on_error() {
        let api = make_api(vec![Module {
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
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

        assert!(
            !dart.contains("weaveffi_free_string(namePtr"),
            "borrowed string param must not be freed via weaveffi_free_string: {dart}"
        );

        let fn_start = dart
            .find("Contact findContact(")
            .expect("findContact wrapper");
        let fn_body = &dart[fn_start..];

        let err_check = fn_body
            .find("_checkError(err)")
            .expect("_checkError in findContact");
        let contact_wrap = fn_body
            .find("Contact._(result)")
            .expect("Contact._ in findContact");
        assert!(
            err_check < contact_wrap,
            "error must be checked before wrapping struct return: {dart}"
        );

        assert!(
            dart.contains("void dispose()") && dart.contains("_destroy"),
            "struct return type should have dispose calling destroy: {dart}"
        );
    }

    #[test]
    fn dart_null_check_on_optional_return() {
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

        let fn_start = dart
            .find("Contact? findContact(")
            .expect("findContact wrapper");
        let fn_body = &dart[fn_start..];

        let null_check = fn_body
            .find("if (result == nullptr) return null")
            .expect("null check in findContact");
        let contact_wrap = fn_body
            .find("Contact._(result)")
            .expect("Contact._ in findContact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check null before wrapping: {dart}"
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
                    doc: None,
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn dart_emits_doc_on_function() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// Performs a thing."), "{dart}");
    }

    #[test]
    fn dart_emits_doc_on_struct() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// An item we track."), "{dart}");
    }

    #[test]
    fn dart_emits_doc_on_enum_variant() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// Kind of item."), "{dart}");
        assert!(dart.contains("/// A small one"), "{dart}");
    }

    #[test]
    fn dart_emits_doc_on_field() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// Stable id"), "{dart}");
    }

    /// A rich (algebraic) enum mirroring `samples/shapes`: a unit variant, an
    /// f64 payload, two f32 payloads, and a (string, u8) payload, plus a plain
    /// sibling enum and functions that take/return the rich enum by handle.
    fn rich_enum_api() -> Api {
        make_api(vec![Module {
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
                    throws: false,
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
                    throws: false,
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
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Circle".into(),
                            value: 1,
                            doc: None,
                            fields: vec![StructField {
                                name: "radius".into(),
                                ty: TypeRef::F64,
                                doc: Some("Radius in points".into()),
                                default: None,
                            }],
                        },
                        EnumVariant {
                            name: "Rectangle".into(),
                            value: 2,
                            doc: None,
                            fields: vec![
                                StructField {
                                    name: "width".into(),
                                    ty: TypeRef::F32,
                                    doc: None,
                                    default: None,
                                },
                                StructField {
                                    name: "height".into(),
                                    ty: TypeRef::F32,
                                    doc: None,
                                    default: None,
                                },
                            ],
                        },
                        EnumVariant {
                            name: "Labeled".into(),
                            value: 3,
                            doc: None,
                            fields: vec![
                                StructField {
                                    name: "label".into(),
                                    ty: TypeRef::StringUtf8,
                                    doc: None,
                                    default: None,
                                },
                                StructField {
                                    name: "count".into(),
                                    ty: TypeRef::U8,
                                    doc: None,
                                    default: None,
                                },
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn rich_enum_is_opaque_class_not_plain_enum() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // The rich enum must NOT be a plain Dart `enum`...
        assert!(
            !dart.contains("enum Shape {"),
            "rich enum must not render as a plain enum: {dart}"
        );
        // ...but an opaque-object wrapper class, exactly like a struct.
        assert!(
            dart.contains("class Shape {"),
            "missing Shape class: {dart}"
        );
        assert!(
            dart.contains("final Pointer<Void> _handle;"),
            "missing opaque handle field: {dart}"
        );
        assert!(
            dart.contains("Shape._(this._handle);"),
            "missing private wrapping constructor: {dart}"
        );
        assert!(
            dart.contains("void dispose()") && dart.contains("weaveffi_shapes_Shape_destroy"),
            "missing dispose()/destroy symbol: {dart}"
        );
        // A plain sibling enum still renders as a plain Dart enum.
        assert!(
            dart.contains("enum Channel {"),
            "plain sibling enum should still render as an enum: {dart}"
        );
    }

    #[test]
    fn rich_enum_tag_reader() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("enum ShapeTag {"),
            "missing typed discriminant enum: {dart}"
        );
        assert!(
            dart.contains("empty(0)")
                && dart.contains("circle(1)")
                && dart.contains("rectangle(2)")
                && dart.contains("labeled(3)"),
            "missing tag discriminants: {dart}"
        );
        assert!(
            dart.contains("ShapeTag get tag"),
            "missing tag discriminant reader: {dart}"
        );
        assert!(
            dart.contains("ShapeTag.fromValue(_weaveffiShapesShapeTag(_handle))"),
            "tag reader must read the C tag symbol: {dart}"
        );
    }

    #[test]
    fn rich_enum_per_variant_factories() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("factory Shape.empty()"),
            "missing unit-variant factory: {dart}"
        );
        assert!(
            dart.contains("factory Shape.circle(double radius)"),
            "missing f64 factory: {dart}"
        );
        assert!(
            dart.contains("factory Shape.rectangle(double width, double height)"),
            "missing two-f32 factory: {dart}"
        );
        assert!(
            dart.contains("factory Shape.labeled(String label, int count)"),
            "missing (string,u8) factory: {dart}"
        );
        // Each factory binds its own per-variant `_new` symbol...
        assert!(
            dart.contains("weaveffi_shapes_Shape_Empty_new")
                && dart.contains("weaveffi_shapes_Shape_Circle_new")
                && dart.contains("weaveffi_shapes_Shape_Rectangle_new")
                && dart.contains("weaveffi_shapes_Shape_Labeled_new"),
            "missing per-variant constructor symbols: {dart}"
        );
        // ...marshals string fields, checks the error, and wraps the handle.
        assert!(
            dart.contains("label.toNativeUtf8()"),
            "labeled factory must marshal its string field: {dart}"
        );
        assert!(
            dart.contains("_checkError(err);") && dart.contains("return Shape._(result);"),
            "factory must check error and wrap the returned handle: {dart}"
        );
    }

    #[test]
    fn rich_enum_per_variant_getters_namespaced() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // Getters are namespaced by variant to avoid collisions; numerics map to
        // int/double (incl. f32 -> double, u8 -> int) and strings decode.
        assert!(
            dart.contains("double get circleRadius"),
            "missing circleRadius getter: {dart}"
        );
        assert!(
            dart.contains("double get rectangleWidth"),
            "missing rectangleWidth getter: {dart}"
        );
        assert!(
            dart.contains("double get rectangleHeight"),
            "missing rectangleHeight getter: {dart}"
        );
        assert!(
            dart.contains("String get labeledLabel"),
            "missing labeledLabel getter: {dart}"
        );
        assert!(
            dart.contains("int get labeledCount"),
            "missing labeledCount getter: {dart}"
        );
        // Getters bind their per-variant C symbols and the string getter decodes.
        assert!(
            dart.contains("weaveffi_shapes_Shape_Circle_get_radius")
                && dart.contains("weaveffi_shapes_Shape_Labeled_get_label"),
            "missing per-variant getter symbols: {dart}"
        );
        assert!(
            dart.contains("final value = result.toDartString();")
                && dart.contains("_weaveffiFreeString(result);"),
            "string getter must decode the C string and free it: {dart}"
        );
        // Carries the per-variant field doc through the namespaced getter.
        assert!(
            dart.contains("/// Radius in points"),
            "variant field doc should be emitted: {dart}"
        );
    }

    #[test]
    fn rich_enum_functions_marshal_opaque_handle() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // Functions taking/returning the rich enum reference it as RichEnum,
        // so they pass the opaque handle in and wrap the handle out unchanged.
        assert!(
            dart.contains("String describe(Shape shape)"),
            "missing describe signature: {dart}"
        );
        assert!(
            dart.contains("Shape scale(Shape shape, double factor)"),
            "missing scale signature: {dart}"
        );
        assert!(
            dart.contains("shape._handle"),
            "a rich-enum argument must marshal as its opaque handle: {dart}"
        );
        assert!(
            dart.contains("return Shape._(result);"),
            "a rich-enum return must wrap the opaque handle: {dart}"
        );
    }

    /// A `kv` module with a declared error domain and a `Store` interface
    /// exercising every member kind: a plain constructor named `new`, a
    /// throwing named constructor, throwing and non-throwing methods, an
    /// async throwing method, an iterator method, and a static.
    fn store_api() -> Api {
        use weaveffi_ir::ir::{ErrorCode, ErrorDomain, InterfaceDef};
        fn f(
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
                cancellable: false,
                deprecated: None,
                since: None,
            }
        }
        fn p(name: &str, ty: TypeRef) -> Param {
            Param {
                name: name.into(),
                ty,
                mutable: false,
                doc: None,
            }
        }
        make_api(vec![Module {
            name: "kv".into(),
            functions: vec![f(
                "inspect",
                vec![p("store", TypeRef::Interface("Store".into()))],
                Some(TypeRef::I64),
                false,
                false,
            )],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: Some("A key-value store.".into()),
                constructors: vec![
                    f("new", vec![p("capacity", TypeRef::I64)], None, false, false),
                    f(
                        "open",
                        vec![p("path", TypeRef::StringUtf8)],
                        None,
                        true,
                        false,
                    ),
                ],
                methods: vec![
                    f(
                        "put",
                        vec![
                            p("key", TypeRef::StringUtf8),
                            p("value", TypeRef::StringUtf8),
                        ],
                        None,
                        true,
                        false,
                    ),
                    f("count", vec![], Some(TypeRef::I64), false, false),
                    f("compact", vec![], Some(TypeRef::I64), true, true),
                    f(
                        "list_keys",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                        true,
                        false,
                    ),
                ],
                statics: vec![f(
                    "default_capacity",
                    vec![],
                    Some(TypeRef::I64),
                    false,
                    false,
                )],
            }],
            errors: Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    ErrorCode {
                        name: "KeyNotFound".into(),
                        code: 1001,
                        message: "key not found".into(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "IoError".into(),
                        code: 1004,
                        message: "I/O failure".into(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn typed_exception_rendering() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        // The domain exception extends the generic brand exception.
        assert!(
            dart.contains("class KvException extends WeaveFFIException {"),
            "missing domain exception: {dart}"
        );
        assert!(
            dart.contains("KvException(super.code, super.message);"),
            "domain exception must forward code and message: {dart}"
        );
        // One subclass per code, preloaded with its stable code and message.
        assert!(
            dart.contains("class KeyNotFoundException extends KvException {"),
            "missing per-code subclass: {dart}"
        );
        assert!(
            dart.contains(
                "KeyNotFoundException([String message = 'key not found']) : super(1001, message);"
            ),
            "per-code subclass must carry its code and default message: {dart}"
        );
        // A code already named `*Error` swaps the suffix rather than stacking.
        assert!(
            dart.contains("class IoException extends KvException {")
                && !dart.contains("IoErrorException"),
            "code exception must swap the Error suffix: {dart}"
        );
        // The mapper covers each code and falls back to the generic exception.
        assert!(
            dart.contains("WeaveFFIException _mapKvException(int code, String message) {"),
            "missing domain mapper: {dart}"
        );
        assert!(
            dart.contains("case 1001:") && dart.contains("return KeyNotFoundException(message);"),
            "mapper must build the per-code subclass: {dart}"
        );
        assert!(
            dart.contains("default:") && dart.contains("return WeaveFFIException(code, message);"),
            "mapper must fall back to the generic exception: {dart}"
        );
        // The per-domain check helper throws through the mapper.
        assert!(
            dart.contains("void _checkKvException(Pointer<_WeaveFFIError> err) {")
                && dart.contains("throw _mapKvException(code, msg);"),
            "missing domain check helper: {dart}"
        );
    }

    #[test]
    fn interface_emits_wrapper_class_with_dispose() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("/// A key-value store.\nclass Store {"),
            "missing documented interface class: {dart}"
        );
        assert!(
            dart.contains("final Pointer<Void> _handle;")
                && dart.contains("Store._(this._handle);"),
            "missing opaque handle plumbing: {dart}"
        );
        let dispose = dart
            .find("class Store {")
            .map(|i| &dart[i..])
            .expect("class body");
        assert!(
            dispose.contains("void dispose() {\n    _weaveffiKvStoreDestroy(_handle);"),
            "dispose must call the interface destroy symbol: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_kv_Store_destroy'"),
            "destroy lookup must bind the C symbol: {dart}"
        );
    }

    #[test]
    fn interface_ctor_new_is_unnamed_factory() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("factory Store(int capacity) {"),
            "missing unnamed factory for ctor `new`: {dart}"
        );
        let body = &dart[dart.find("factory Store(int capacity)").expect("ctor body")..];
        assert!(
            body.contains("_weaveffiKvStoreNew(capacity, err)"),
            "ctor must call its member symbol: {dart}"
        );
        // Non-throwing ctor still traps through the generic check.
        assert!(
            body.contains("_checkError(err);"),
            "plain ctor must use the generic check: {dart}"
        );
        assert!(
            body.contains("return Store._(result);"),
            "ctor must adopt the owned handle: {dart}"
        );
    }

    #[test]
    fn interface_secondary_ctor_is_named_factory() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("factory Store.open(String path) {"),
            "missing named factory: {dart}"
        );
        let body = &dart[dart.find("factory Store.open(").expect("open body")..];
        assert!(
            body.contains("_weaveffiKvStoreOpen(pathPtr, err)"),
            "named factory must call its member symbol: {dart}"
        );
        assert!(
            body.contains("_checkKvException(err);"),
            "throwing factory must use the domain check: {dart}"
        );
        assert!(
            body.contains("return Store._(result);"),
            "named factory must adopt the owned handle: {dart}"
        );
        // The throwing ctor documents the thrown domain exception.
        assert!(
            dart.contains("/// Throws [KvException] on domain errors.\n  factory Store.open("),
            "throwing ctor must note the thrown type: {dart}"
        );
    }

    #[test]
    fn interface_methods_pass_self_handle() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        // Throwing instance method: `_handle` leads the C argument list.
        assert!(
            dart.contains("void put(String key, String value) {"),
            "missing instance method: {dart}"
        );
        assert!(
            dart.contains("_weaveffiKvStorePut(_handle, keyPtr, valuePtr, err);"),
            "method must pass _handle as the leading argument: {dart}"
        );
        let put_body = &dart[dart.find("void put(").expect("put body")..];
        assert!(
            put_body.contains("_checkKvException(err);"),
            "throwing method must use the domain check: {dart}"
        );
        // Non-throwing method uses the generic check.
        let count_body = &dart[dart.find("int count()").expect("count body")..];
        assert!(
            count_body.contains("_weaveffiKvStoreCount(_handle, err)")
                && count_body.contains("_checkError(err);"),
            "plain method must call with _handle and check generically: {dart}"
        );
    }

    #[test]
    fn interface_async_method_maps_typed_error() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("Future<int> compact() {"),
            "missing async method: {dart}"
        );
        assert!(
            dart.contains(
                "_weaveffiKvStoreCompactAsync(_handle, callable.nativeFunction, nullptr);"
            ),
            "async launcher must lead with _handle: {dart}"
        );
        assert!(
            dart.contains("completer.completeError(_mapKvException(code, msg));"),
            "async throwing method must complete with the typed exception: {dart}"
        );
    }

    #[test]
    fn interface_iterator_method_checks_domain() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("Iterable<String> listKeys() sync* {"),
            "missing lazy iterator method: {dart}"
        );
        assert!(
            dart.contains("_weaveffiKvStoreListKeys(_handle, err)"),
            "iterator launch must lead with _handle: {dart}"
        );
        let body = &dart[dart
            .find("Iterable<String> listKeys()")
            .expect("listKeys body")..];
        assert!(
            body.contains("_checkKvException(err);"),
            "throwing iterator must route launch and next through the domain check: {dart}"
        );
    }

    /// The `iter<T>` wrapper must be a lazy `sync*` generator: one producer
    /// `next` call per yielded element, no hidden drain into a list, and a
    /// `try`/`finally` that destroys the handle exactly once (nulling it) on
    /// exhaustion, error, or generator teardown.
    #[test]
    fn iterator_wrapper_is_lazy_sync_star() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        let body = &dart[dart
            .find("Iterable<String> listKeys() sync* {")
            .expect("sync* wrapper")..];
        let body = &body[..body.find("\n  }").expect("member end")];
        // One `next` per consumer step, yielded straight out of the loop.
        assert!(
            body.contains("while (_weaveffiKvStoreListKeysIteratorNext(iter, outItem, err) != 0) {"),
            "missing per-element next loop: {body}"
        );
        assert!(body.contains("yield item;"), "missing yield: {body}");
        assert!(
            !body.contains(".add(") && !body.contains("return items;"),
            "iterator must not drain into a list: {body}"
        );
        // Destroy exactly once, guarded and nulled, from the finally block.
        assert!(body.contains("} finally {"), "missing finally: {body}");
        assert!(
            body.contains("if (iter != nullptr) {")
                && body.contains("_weaveffiKvStoreListKeysIteratorDestroy(iter);")
                && body.contains("iter = nullptr;"),
            "finally must destroy once and null the handle: {body}"
        );
        // String elements are copied then freed per ElemFree::String.
        assert!(
            body.contains("final item = itemPtr.toDartString();")
                && body.contains("_weaveffiFreeString(itemPtr);"),
            "string elements must be copied then freed: {body}"
        );
    }

    /// Abandoned iterations (a broken `for`, `first`, `take`) never resume a
    /// `sync*` body, so its `finally` cannot run; the wrapper attaches a
    /// `NativeFinalizer` backstop to a generator-local anchor and detaches it
    /// before the eager destroy so double-destroy is impossible.
    #[test]
    fn iterator_wrapper_has_finalizer_backstop() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("final class _IteratorLifetime implements Finalizable {}"),
            "missing iterator lifetime anchor class: {dart}"
        );
        assert!(
            dart.contains(
                "final _weaveffiKvStoreListKeysIteratorDestroyFinalizer = NativeFinalizer("
            ),
            "missing NativeFinalizer over the destroy symbol: {dart}"
        );
        let body = &dart[dart
            .find("Iterable<String> listKeys() sync* {")
            .expect("sync* wrapper")..];
        let body = &body[..body.find("\n  }").expect("member end")];
        assert!(
            body.contains(
                "_weaveffiKvStoreListKeysIteratorDestroyFinalizer.attach(anchor, iter, detach: anchor);"
            ),
            "launch must attach the finalizer backstop: {body}"
        );
        assert!(
            body.contains("_weaveffiKvStoreListKeysIteratorDestroyFinalizer.detach(anchor);"),
            "eager destroy must detach the backstop first: {body}"
        );
    }

    /// A free function returning `iter<record>` yields adopted wrapper
    /// objects (the caller disposes each) and documents the streaming and
    /// disposal contract.
    #[test]
    fn iterator_of_records_yields_adopted_wrappers() {
        let api = make_api(vec![Module {
            name: "kv".into(),
            functions: vec![Function {
                name: "entries".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
                doc: Some("Streams every entry.".into()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Entry".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi", "kv.yml");
        assert!(
            dart.contains("Iterable<Entry> entries() sync* {"),
            "missing record iterator wrapper: {dart}"
        );
        assert!(
            dart.contains("yield Entry._(outItem.value);"),
            "record elements must be adopted by their wrapper: {dart}"
        );
        // Non-throwing: launch and next errors trap via the generic check.
        let body = &dart[dart.find("Iterable<Entry> entries()").expect("body")..];
        assert!(
            body.contains("_checkError(err);"),
            "trap-strategy iterator must use the generic check: {dart}"
        );
        // The generated doc states the streaming and disposal contract.
        assert!(
            dart.contains("/// Returns a lazy [Iterable]:"),
            "missing streaming doc: {dart}"
        );
        assert!(
            dart.contains("/// Each yielded element is owned by the caller:"),
            "missing element ownership doc: {dart}"
        );
    }

    #[test]
    fn interface_static_is_static_method() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("static int defaultCapacity() {"),
            "missing static method: {dart}"
        );
        let body = &dart[dart
            .find("static int defaultCapacity()")
            .expect("static body")..];
        assert!(
            body.contains("_weaveffiKvStoreDefaultCapacity(err)"),
            "static must call its member symbol without a self slot: {dart}"
        );
    }

    #[test]
    fn interface_param_passes_borrowed_handle() {
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        // Free function taking the interface: the class is the Dart type and
        // the call borrows its handle without wrapping or disposing.
        assert!(
            dart.contains("int inspect(Store store) {"),
            "missing interface-typed param signature: {dart}"
        );
        assert!(
            dart.contains("_weaveffiKvInspect(store._handle, err)"),
            "interface param must pass ._handle: {dart}"
        );
    }

    #[test]
    fn throws_split_on_free_functions() {
        use weaveffi_ir::ir::{ErrorCode, ErrorDomain};
        let api = make_api(vec![Module {
            name: "calc".into(),
            functions: vec![
                Function {
                    name: "div".into(),
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
                    throws: true,
                    r#async: false,
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
                    throws: false,
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
            interfaces: vec![],
            errors: Some(ErrorDomain {
                name: "CalcError".into(),
                codes: vec![ErrorCode {
                    name: "DivisionByZero".into(),
                    code: 1,
                    message: "Division by zero".into(),
                    doc: None,
                }],
            }),
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi", "calc.yml");
        // throws: true routes the slot through the domain check and says so.
        let div_body = &dart[dart.find("int div(int a, int b)").expect("div body")..];
        assert!(
            div_body.contains("_checkCalcException(err);"),
            "throwing fn must use the domain check: {dart}"
        );
        assert!(
            dart.contains("/// Throws [CalcException] on domain errors.\nint div(int a, int b) {"),
            "throwing fn must note the thrown type: {dart}"
        );
        // throws: false keeps the generic check for panics and marshalling.
        let add_body = &dart[dart.find("int add(int a, int b)").expect("add body")..];
        assert!(
            add_body.contains("_checkError(err);"),
            "plain fn must check generically: {dart}"
        );
        assert!(
            !add_body[..add_body.find('}').unwrap_or(add_body.len())]
                .contains("_checkCalcException"),
            "plain fn must not use the domain check: {dart}"
        );
    }

    #[test]
    fn strip_module_prefix_defaults_to_true() {
        assert!(
            DartConfig::default().strip_module_prefix,
            "stripping must be the default"
        );
        let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
        assert!(
            dart.contains("int inspect(Store store) {") && !dart.contains("int kvInspect("),
            "default naming must strip the module prefix: {dart}"
        );
    }

    /// Mirrors the `cli_dart.rs` expectations for `samples/contacts` by
    /// rendering the sample directly; kept here because the CLI binary cannot
    /// build while other generator crates are mid-overhaul.
    #[test]
    fn contacts_sample_renders_interface_and_domain() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest_dir).join("../../samples/contacts/contacts.yml");
        let src = std::fs::read_to_string(path).expect("contacts sample readable");
        let mut api =
            weaveffi_ir::parse::parse_api_str(&src, "yaml").expect("contacts sample parses");
        // Generators run strictly post-resolution: rewrite every parsed
        // `Named` reference into its resolved kind first, as the CLI does.
        weaveffi_core::validate::resolve_type_refs(&mut api);
        let dart = render_dart_module(&api, "weaveffi", "contacts.yml");
        assert!(
            dart.contains("enum ContactType {"),
            "missing ContactType enum: {dart}"
        );
        assert!(dart.contains("class Contact {"), "missing Contact: {dart}");
        assert!(
            dart.contains("class ContactBook {") && dart.contains("factory ContactBook() {"),
            "missing ContactBook interface: {dart}"
        );
        assert!(
            dart.contains("class ContactsException extends WeaveFFIException {"),
            "missing ContactsException: {dart}"
        );
        assert!(
            dart.contains("weaveffi_contacts_ContactBook_add"),
            "missing ContactBook add member symbol: {dart}"
        );
    }

    /// One-function module helper for the ownership-audit tests below.
    fn returning(name: &str, returns: TypeRef) -> Api {
        make_api(vec![simple_module(vec![Function {
            name: name.into(),
            params: vec![],
            returns: Some(returns),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])])
    }

    #[test]
    fn bytes_return_copies_then_frees_buffer() {
        let dart = render_dart_module(
            &returning("blob", TypeRef::Bytes),
            "weaveffi",
            "weaveffi.yml",
        );
        assert!(
            dart.contains("final bytes = List<int>.generate(n, (i) => result[i]);"),
            "bytes must be copied: {dart}"
        );
        assert!(
            dart.contains("_weaveffiFreeBytes(result, n);"),
            "bytes buffer must be freed after copying: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_free_bytes'"),
            "missing weaveffi_free_bytes lookup: {dart}"
        );
    }

    #[test]
    fn string_list_return_frees_elements_and_buffer() {
        let dart = render_dart_module(
            &returning("names", TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "weaveffi",
            "weaveffi.yml",
        );
        assert!(
            dart.contains("final arr = result.cast<Pointer<Utf8>>();"),
            "missing element cast: {dart}"
        );
        // Each copied string element is released, then the array itself.
        assert!(
            dart.contains("_weaveffiFreeString(arr[i]);"),
            "string elements must be freed after copying: {dart}"
        );
        assert!(
            dart.contains("_weaveffiFreeBytes(result.cast(), n * sizeOf<Pointer<Utf8>>());"),
            "array buffer must be freed: {dart}"
        );
    }

    #[test]
    fn record_list_return_adopts_elements_and_frees_buffer() {
        let dart = render_dart_module(
            &returning("all", TypeRef::List(Box::new(TypeRef::Record("Item".into())))),
            "weaveffi",
            "weaveffi.yml",
        );
        // Object elements transfer to the caller (no per-element free); only
        // the array buffer is released.
        assert!(
            dart.contains("List<Item>.generate(n, (i) => Item._(arr[i]));"),
            "record elements must be adopted: {dart}"
        );
        assert!(
            !dart.contains("_weaveffiFreeString(arr[i]);"),
            "record elements must not be string-freed: {dart}"
        );
        assert!(
            dart.contains("_weaveffiFreeBytes(result.cast(), n * sizeOf<Pointer<Void>>());"),
            "array buffer must be freed: {dart}"
        );
    }

    #[test]
    fn map_return_frees_string_elements_and_parallel_arrays() {
        let dart = render_dart_module(
            &returning(
                "tally",
                TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            ),
            "weaveffi",
            "weaveffi.yml",
        );
        assert!(
            dart.contains("_weaveffiFreeString(keys[i]);"),
            "string keys must be freed after copying: {dart}"
        );
        assert!(
            dart.contains("_weaveffiFreeBytes(keys.cast(), n * sizeOf<Pointer<Utf8>>());"),
            "keys array must be freed: {dart}"
        );
        assert!(
            dart.contains("_weaveffiFreeBytes(vals.cast(), n * sizeOf<Int32>());"),
            "values array must be freed: {dart}"
        );
    }

    #[test]
    fn boxed_optional_scalar_return_is_freed() {
        let dart = render_dart_module(
            &returning("level", TypeRef::Optional(Box::new(TypeRef::I64))),
            "weaveffi",
            "weaveffi.yml",
        );
        assert!(
            dart.contains("if (result == nullptr) return null;"),
            "missing null check: {dart}"
        );
        assert!(
            dart.contains("final value = result.value;")
                && dart.contains("_weaveffiFreeBytes(result.cast(), sizeOf<Int64>());"),
            "boxed scalar must be dereferenced then freed: {dart}"
        );
    }

    /// Async result buffers are borrowed for the callback's duration: the
    /// wrapper deep-copies them inside the callback and never frees them.
    #[test]
    fn async_buffer_results_copy_and_never_free() {
        let api = make_api(vec![simple_module(vec![
            Function {
                name: "fetch_names".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "fetch_blob".into(),
                params: vec![],
                returns: Some(TypeRef::Bytes),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ])]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Future<List<String>> fetchNames()"),
            "missing async list wrapper: {dart}"
        );
        assert!(
            dart.contains(": List<String>.generate(resultLen, (i) => result[i].toDartString()));"),
            "async list result must be copied element-wise: {dart}"
        );
        assert!(
            dart.contains(": List<int>.generate(resultLen, (i) => result[i]));"),
            "async bytes result must be copied: {dart}"
        );
        // Borrowed: the callback must not release the producer's buffers.
        let cb = &dart[dart.find("Future<List<String>> fetchNames()").expect("wrapper")..];
        let cb = &cb[..cb.find("\n}").expect("end")];
        assert!(
            !cb.contains("_weaveffiFree"),
            "async callback must never free borrowed result buffers: {cb}"
        );
    }

    #[test]
    fn strip_module_prefix_can_be_disabled() {
        let api = store_api();
        let model = BindingModel::build(&api, "weaveffi");
        let config = DartConfig {
            prefix: Some("weaveffi".into()),
            input_basename: Some("kv.yml".into()),
            strip_module_prefix: false,
            ..DartConfig::default()
        };
        let dart = super::render_dart_module(&api, &model, &config);
        assert!(
            dart.contains("int kvInspect(Store store) {"),
            "disabled stripping must keep the module prefix: {dart}"
        );
        // Interface members are namespaced by their class, never prefixed.
        assert!(
            dart.contains("factory Store.open(String path) {"),
            "interface members must not gain a module prefix: {dart}"
        );
    }
}
