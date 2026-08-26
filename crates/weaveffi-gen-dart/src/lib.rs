//! Dart (`dart:ffi`) binding generator for WeaveFFI.
//!
//! Emits a Dart package (`pubspec.yaml` + library) with `dart:ffi`
//! bindings over the C ABI for use in Flutter and Dart projects.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.
//!
//! Records and rich enums are value types: they render as plain Dart classes
//! (a sealed hierarchy for a rich enum) and cross the ABI serialized in the
//! WeaveFFI value-buffer format as one `(ptr, len)` pair. The generated
//! library ships a small private buffer writer/reader implementing that
//! format, plus one pack and one unpack routine per record and rich enum.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::is_buffered;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::{elem_free, ElemFree, ErrorStrategy};
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

/// The idiomatic Dart type a [`TypeRef`] surfaces as.
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

/// The bare local Dart class name of a (possibly dot-qualified) user type.
fn dart_class(name: &str) -> String {
    local_type_name(name).to_upper_camel_case()
}

/// dart:ffi (native, dart) types of a leaf scalar passed by value. `Bool` is
/// one byte, matching the producer's C `bool`, so by-value slots stay honest.
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

// ── ABI slot typing ──

/// The (native, dart) FFI typedef slot pairs a single input parameter expands
/// into, mirroring the C ABI: a buffered value is one borrowed
/// `(const uint8_t*, size_t)` pair; bytes fan out to `(ptr, len)`; strings,
/// interfaces, and typed handles stay one pointer slot; everything else is a
/// by-value scalar.
fn input_slots(ty: &TypeRef) -> Vec<(String, String)> {
    let ptr = |s: &str| (s.to_string(), s.to_string());
    if is_buffered(ty) {
        return vec![ptr("Pointer<Uint8>"), ("Size".into(), "int".into())];
    }
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![ptr("Pointer<Uint8>"), ("Size".into(), "int".into())]
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![ptr("Pointer<Utf8>")],
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) => vec![ptr("Pointer<Void>")],
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable object pointer, null meaning none.
        TypeRef::Optional(inner) => input_slots(inner),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        _ => {
            let (n, d) = scalar_ffi(ty);
            vec![(n.into(), d.into())]
        }
    }
}

/// The FFI return type (native, dart) of a call symbol. Buffered and bytes
/// returns come back as a producer-allocated `Pointer<Uint8>`; strings as
/// `Pointer<Utf8>`; interfaces and typed handles as opaque pointers.
fn return_ffi(ty: &TypeRef) -> (String, String) {
    let ptr = |s: &str| (s.to_string(), s.to_string());
    if is_buffered(ty) {
        return ptr("Pointer<Uint8>");
    }
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => ptr("Pointer<Uint8>"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ptr("Pointer<Utf8>"),
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) => ptr("Pointer<Void>"),
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => return_ffi(inner),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Iterator(_) => {
            unreachable!("iterator returns are lowered through IteratorBinding")
        }
        _ => {
            let (n, d) = scalar_ffi(ty);
            (n.into(), d.into())
        }
    }
}

/// The trailing FFI typedef slots (native, dart) a return type contributes:
/// bytes and every buffered return add a single `size_t* out_len`.
fn return_out_slots(ty: &TypeRef) -> Vec<(String, String)> {
    if is_buffered(ty) || matches!(ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
        vec![("Pointer<Size>".into(), "Pointer<Size>".into())]
    } else {
        vec![]
    }
}

/// Whether a return owes the caller a decode from a producer-allocated
/// `(ptr, out_len)` buffer (a bytes return or any buffered value).
fn returns_buffer(ty: &TypeRef) -> bool {
    is_buffered(ty) || matches!(ty, TypeRef::Bytes | TypeRef::BorrowedBytes)
}

// ── Value-buffer encode/decode codegen ──

/// The `_pack{Name}` helper name for a (possibly dot-qualified) record or
/// rich-enum reference.
fn pack_fn(name: &str) -> String {
    format!("_pack{}", dart_class(name))
}

/// The `_unpack{Name}` helper name for a (possibly dot-qualified) record or
/// rich-enum reference.
fn unpack_fn(name: &str) -> String {
    format!("_unpack{}", dart_class(name))
}

/// Mint a fresh `t{n}` temporary name.
fn fresh(tmp: &mut usize) -> String {
    let n = *tmp;
    *tmp += 1;
    format!("t{n}")
}

/// The Dart expression decoding one value of `ty` from the reader named `r`.
///
/// Optionals, lists, and maps recurse; records and rich enums call their
/// generated `_unpack{Name}` helper. All read expressions evaluate strictly
/// left to right, so composing them preserves the wire order.
fn read_expr(r: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::Bool => format!("{r}.readBool()"),
        TypeRef::I8 => format!("{r}.readInt8()"),
        TypeRef::I16 => format!("{r}.readInt16()"),
        TypeRef::I32 => format!("{r}.readInt32()"),
        TypeRef::I64 => format!("{r}.readInt64()"),
        TypeRef::U8 => format!("{r}.readUint8()"),
        TypeRef::U16 => format!("{r}.readUint16()"),
        TypeRef::U32 => format!("{r}.readUint32()"),
        TypeRef::U64 => format!("{r}.readUint64()"),
        TypeRef::F32 => format!("{r}.readFloat32()"),
        TypeRef::F64 => format!("{r}.readFloat64()"),
        TypeRef::Handle => format!("{r}.readUint64()"),
        TypeRef::TypedHandle(n) => format!(
            "{}._(Pointer<Void>.fromAddress({r}.readUint64()))",
            dart_class(n)
        ),
        TypeRef::Enum(n) => format!("{}.fromValue({r}.readInt32())", dart_class(n)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{r}.readString()"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => format!("{r}.readBytes()"),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => format!("{}({r})", unpack_fn(n)),
        TypeRef::Optional(inner) => {
            format!("({r}.readOptionFlag() ? {} : null)", read_expr(r, inner))
        }
        TypeRef::List(inner) => format!(
            "List<{}>.generate({r}.readLength(), (_) => {})",
            dart_type(inner),
            read_expr(r, inner)
        ),
        TypeRef::Map(k, v) => format!(
            "<{}, {}>{{ for (var i = {r}.readLength(); i > 0; i--) {}: {} }}",
            dart_type(k),
            dart_type(v),
            read_expr(r, k),
            read_expr(r, v)
        ),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("interfaces and iterators never appear inside a value buffer")
        }
    }
}

/// Emit the statements encoding `expr` (a value of `ty`) into the writer
/// named `wr`. Optionals, lists, and maps recurse through fresh `t{n}`
/// temporaries; records and rich enums call their generated `_pack{Name}`
/// helper.
fn write_stmts(w: &mut CodeWriter, wr: &str, expr: &str, ty: &TypeRef, tmp: &mut usize) {
    match ty {
        TypeRef::Bool => {
            w.line(format!("{wr}.writeBool({expr});"));
        }
        TypeRef::I8 => {
            w.line(format!("{wr}.writeInt8({expr});"));
        }
        TypeRef::I16 => {
            w.line(format!("{wr}.writeInt16({expr});"));
        }
        TypeRef::I32 => {
            w.line(format!("{wr}.writeInt32({expr});"));
        }
        TypeRef::I64 => {
            w.line(format!("{wr}.writeInt64({expr});"));
        }
        TypeRef::U8 => {
            w.line(format!("{wr}.writeUint8({expr});"));
        }
        TypeRef::U16 => {
            w.line(format!("{wr}.writeUint16({expr});"));
        }
        TypeRef::U32 => {
            w.line(format!("{wr}.writeUint32({expr});"));
        }
        TypeRef::U64 => {
            w.line(format!("{wr}.writeUint64({expr});"));
        }
        TypeRef::F32 => {
            w.line(format!("{wr}.writeFloat32({expr});"));
        }
        TypeRef::F64 => {
            w.line(format!("{wr}.writeFloat64({expr});"));
        }
        TypeRef::Handle => {
            w.line(format!("{wr}.writeUint64({expr});"));
        }
        TypeRef::TypedHandle(_) => {
            w.line(format!("{wr}.writeUint64({expr}._handle.address);"));
        }
        TypeRef::Enum(_) => {
            w.line(format!("{wr}.writeInt32({expr}.value);"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{wr}.writeString({expr});"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("{wr}.writeBytes({expr});"));
        }
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            w.line(format!("{}({wr}, {expr});", pack_fn(n)));
        }
        TypeRef::Optional(inner) => {
            let t = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("if ({t} == null) {{"));
            w.scope(|w| {
                w.line(format!("{wr}.writeOptionFlag(false);"));
            });
            w.line("} else {");
            w.scope(|w| {
                w.line(format!("{wr}.writeOptionFlag(true);"));
                write_stmts(w, wr, &t, inner, &mut *tmp);
            });
            w.line("}");
        }
        TypeRef::List(inner) => {
            let t = fresh(tmp);
            let e = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeLength({t}.length);"));
            w.line(format!("for (final {e} in {t}) {{"));
            w.scope(|w| {
                write_stmts(w, wr, &e, inner, &mut *tmp);
            });
            w.line("}");
        }
        TypeRef::Map(k, v) => {
            let t = fresh(tmp);
            let e = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeLength({t}.length);"));
            w.line(format!("for (final {e} in {t}.entries) {{"));
            w.scope(|w| {
                write_stmts(w, wr, &format!("{e}.key"), k, &mut *tmp);
                write_stmts(w, wr, &format!("{e}.value"), v, &mut *tmp);
            });
            w.line("}");
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("interfaces and iterators never appear inside a value buffer")
        }
    }
}

/// Emit the private Dart value-buffer runtime: the little-endian, packed
/// writer and reader plus the staging/copy helpers wrappers use to move
/// encodings across the boundary.
fn render_buffer_runtime(out: &mut String) {
    out.push_str(
        r#"
// ── WeaveFFI value-buffer runtime ──
// Records, rich enums, optionals, lists, maps, and error payloads cross the
// C ABI serialized in this little-endian, packed format. A malformed buffer
// is a producer/consumer contract violation, never a typed domain error.

Never _bufferError(String context) =>
    throw StateError('malformed WeaveFFI value buffer: $context');

// Copies a borrowed native (ptr, len) buffer into Dart-owned memory.
Uint8List _copyNativeBytes(Pointer<Uint8> ptr, int len) =>
    ptr == nullptr ? Uint8List(0) : Uint8List.fromList(ptr.asTypedList(len));

// Stages an encoding into native memory for a borrowed (ptr, len) argument;
// the caller frees the pointer after the call returns.
Pointer<Uint8> _stageBytes(Uint8List bytes) {
  final ptr = calloc<Uint8>(bytes.isEmpty ? 1 : bytes.length);
  if (bytes.isNotEmpty) ptr.asTypedList(bytes.length).setAll(0, bytes);
  return ptr;
}

final class _BufferWriter {
  Uint8List _buf = Uint8List(64);
  ByteData? _view;
  int _len = 0;

  ByteData get _data => _view ??= ByteData.sublistView(_buf);

  void _ensure(int extra) {
    if (_len + extra <= _buf.length) return;
    var cap = _buf.length * 2;
    while (cap < _len + extra) {
      cap *= 2;
    }
    final next = Uint8List(cap);
    next.setRange(0, _len, _buf);
    _buf = next;
    _view = null;
  }

  Uint8List takeBytes() => Uint8List.sublistView(_buf, 0, _len);

  void writeBool(bool v) => writeUint8(v ? 1 : 0);

  void writeOptionFlag(bool present) => writeUint8(present ? 1 : 0);

  void writeInt8(int v) {
    _ensure(1);
    _data.setInt8(_len, v);
    _len += 1;
  }

  void writeUint8(int v) {
    _ensure(1);
    _data.setUint8(_len, v);
    _len += 1;
  }

  void writeInt16(int v) {
    _ensure(2);
    _data.setInt16(_len, v, Endian.little);
    _len += 2;
  }

  void writeUint16(int v) {
    _ensure(2);
    _data.setUint16(_len, v, Endian.little);
    _len += 2;
  }

  void writeInt32(int v) {
    _ensure(4);
    _data.setInt32(_len, v, Endian.little);
    _len += 4;
  }

  void writeUint32(int v) {
    _ensure(4);
    _data.setUint32(_len, v, Endian.little);
    _len += 4;
  }

  void writeInt64(int v) {
    _ensure(8);
    _data.setInt64(_len, v, Endian.little);
    _len += 8;
  }

  void writeUint64(int v) {
    _ensure(8);
    _data.setUint64(_len, v, Endian.little);
    _len += 8;
  }

  void writeFloat32(double v) {
    _ensure(4);
    _data.setFloat32(_len, v, Endian.little);
    _len += 4;
  }

  void writeFloat64(double v) {
    _ensure(8);
    _data.setFloat64(_len, v, Endian.little);
    _len += 8;
  }

  void writeLength(int v) => writeUint32(v);

  void writeString(String v) {
    final bytes = utf8.encode(v);
    writeLength(bytes.length);
    _ensure(bytes.length);
    _buf.setRange(_len, _len + bytes.length, bytes);
    _len += bytes.length;
  }

  void writeBytes(List<int> v) {
    writeLength(v.length);
    _ensure(v.length);
    _buf.setRange(_len, _len + v.length, v);
    _len += v.length;
  }
}

final class _BufferReader {
  final Uint8List _buf;
  final ByteData _data;
  int _pos = 0;

  _BufferReader(Uint8List buf)
      : _buf = buf,
        _data = ByteData.sublistView(buf);

  int get _remaining => _buf.length - _pos;

  void _require(int n, String context) {
    if (_remaining < n) _bufferError(context);
  }

  bool readBool() {
    _require(1, 'bool');
    final b = _buf[_pos++];
    if (b > 1) _bufferError('bool byte out of range');
    return b == 1;
  }

  bool readOptionFlag() {
    _require(1, 'option flag');
    final b = _buf[_pos++];
    if (b > 1) _bufferError('option flag byte out of range');
    return b == 1;
  }

  int readInt8() {
    _require(1, 'i8');
    return _data.getInt8(_pos++);
  }

  int readUint8() {
    _require(1, 'u8');
    return _buf[_pos++];
  }

  int readInt16() {
    _require(2, 'i16');
    final v = _data.getInt16(_pos, Endian.little);
    _pos += 2;
    return v;
  }

  int readUint16() {
    _require(2, 'u16');
    final v = _data.getUint16(_pos, Endian.little);
    _pos += 2;
    return v;
  }

  int readInt32() {
    _require(4, 'i32');
    final v = _data.getInt32(_pos, Endian.little);
    _pos += 4;
    return v;
  }

  int readUint32() {
    _require(4, 'u32');
    final v = _data.getUint32(_pos, Endian.little);
    _pos += 4;
    return v;
  }

  int readInt64() {
    _require(8, 'i64');
    final v = _data.getInt64(_pos, Endian.little);
    _pos += 8;
    return v;
  }

  int readUint64() {
    _require(8, 'u64');
    final v = _data.getUint64(_pos, Endian.little);
    _pos += 8;
    return v;
  }

  double readFloat32() {
    _require(4, 'f32');
    final v = _data.getFloat32(_pos, Endian.little);
    _pos += 4;
    return v;
  }

  double readFloat64() {
    _require(8, 'f64');
    final v = _data.getFloat64(_pos, Endian.little);
    _pos += 8;
    return v;
  }

  int readLength() {
    final n = readUint32();
    if (n > _remaining) _bufferError('length prefix exceeds remaining buffer');
    return n;
  }

  String readString() {
    final n = readLength();
    final s = utf8.decode(Uint8List.sublistView(_buf, _pos, _pos + n));
    _pos += n;
    return s;
  }

  Uint8List readBytes() {
    final n = readLength();
    final b = Uint8List.fromList(Uint8List.sublistView(_buf, _pos, _pos + n));
    _pos += n;
    return b;
  }

  void expectEnd() {
    if (_remaining != 0) _bufferError('trailing bytes after value');
  }
}
"#,
    );
}

/// Emit the dart:ffi typedef pair and `lookupFunction` binding for one C
/// symbol.
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
    if has_async {
        out.push_str("import 'dart:async';\n");
    }
    out.push_str("import 'dart:convert';\n");
    out.push_str("import 'dart:ffi';\n");
    out.push_str("import 'dart:io' show Platform;\n");
    out.push_str("import 'dart:typed_data';\n\n");
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

    // The error slot every fallible call writes. `payloadPtr`/`payloadLen`
    // hold the matched error code's fields serialized in the value-buffer
    // format (null when the code declares no fields); `weaveffi_error_clear`
    // releases both the message and the payload.
    out.push_str("final class _WeaveFFIError extends Struct {\n");
    out.push_str("  @Int32()\n");
    out.push_str("  external int code;\n");
    out.push_str("  external Pointer<Utf8> message;\n");
    out.push_str("  external Pointer<Uint8> payloadPtr;\n");
    out.push_str("  @Size()\n");
    out.push_str("  external int payloadLen;\n");
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
    // buffer (bytes and serialized value buffers) with `weaveffi_free_bytes`.
    // The runtime always exports these under the canonical `weaveffi_` names,
    // like `weaveffi_error_clear`.
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

    render_buffer_runtime(&mut out);

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        out.push_str("\n// Live listener trampolines by subscription id. Holding the\n");
        out.push_str("// NativeCallable here keeps its native thunk alive until unregistered.\n");
        out.push_str("final Map<int, NativeCallable> _listenerCallables = {};\n");
    }

    let has_iterators = model.modules.iter().any(|m| {
        m.callables()
            .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    });
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
        }
        for i in &module.interfaces {
            render_interface(&mut out, module, i);
        }
        for cb in &module.callbacks {
            render_callback_typedef(&mut out, cb);
        }
        for l in &module.listeners {
            render_listener(&mut out, module, l, config.strip_module_prefix);
        }
        for f in &module.functions {
            render_function(&mut out, module, f, config.strip_module_prefix);
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
    /// already-captured `code`/`msg` (and, for a domain error, `payload`)
    /// locals.
    fn map_expr(&self) -> String {
        match self.thrown_exception() {
            Some(exc) => format!("_map{exc}(code, msg, payload)"),
            None => "WeaveFFIException(code, msg)".to_string(),
        }
    }
}

/// Render one module's declared error domain: the domain exception extending
/// the generic [`errors::EXCEPTION_BRAND`], one exception subclass per code
/// carrying its stable code, default message, and any decoded payload fields,
/// and the `_map`/`_check` helpers that throwing wrappers route their out-err
/// slots through. When a code declares payload fields, the mapper decodes the
/// error's payload buffer into the exception's typed properties. Unknown
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
            if c.fields.is_empty() {
                w.line(format!(
                    "{class}([String message = '{message}']) : super({}, message);",
                    c.value
                ));
            } else {
                for f in &c.fields {
                    let mut fd = String::new();
                    emit_doc(&mut fd, &f.doc, "  ");
                    w.raw(fd);
                    w.line(format!(
                        "final {} {};",
                        dart_type(&f.ty),
                        f.name.to_lower_camel_case()
                    ));
                }
                w.blank();
                let params: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", f.name.to_lower_camel_case()))
                    .collect();
                w.line(format!(
                    "{class}({}, [String message = '{message}']) : super({}, message);",
                    params.join(", "),
                    c.value
                ));
            }
        });
    }

    w.blank();
    w.line(format!(
        "{brand} _map{exc}(int code, String message, Uint8List payload) {{"
    ));
    w.scope(|w| {
        w.block("switch (code) {", "}", |w| {
            for c in &eb.codes {
                let class = dart_exception_name(&c.name);
                if c.fields.is_empty() {
                    w.line(format!("case {}:", c.value));
                    w.scope(|w| {
                        w.line(format!("return {class}(message);"));
                    });
                } else {
                    // Braces give each payload-decoding case its own scope,
                    // so the reader and field locals never collide between
                    // cases (a Dart switch otherwise shares one scope).
                    w.line(format!("case {}: {{", c.value));
                    w.scope(|w| {
                        w.line("final r = _BufferReader(payload);");
                        let mut args: Vec<String> = Vec::new();
                        for (i, f) in c.fields.iter().enumerate() {
                            w.line(format!("final v{i} = {};", read_expr("r", &f.ty)));
                            args.push(format!("v{i}"));
                        }
                        w.line("r.expectEnd();");
                        w.line(format!("return {class}({}, message);", args.join(", ")));
                    });
                    w.line("}");
                }
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
                w.line("final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);");
                w.line("_weaveffiErrorClear(err);");
                w.line(format!("throw _map{exc}(code, msg, payload);"));
            });
        },
    );
    out.push_str(&w.finish());
}

/// The [`ErrCtx`] for one callable of a module: its [`ErrorStrategy`] paired
/// with the exception class of the domain in effect (own or inherited).
fn err_ctx<'a>(f: &FnBinding, exception: Option<&'a str>) -> ErrCtx<'a> {
    ErrCtx {
        throws: matches!(f.error_strategy(), ErrorStrategy::Throws),
        exception,
    }
}

/// Render one interface as an opaque-object wrapper class: it owns the C
/// handle behind a private `_handle`, frees it once in `dispose()` via the
/// interface's destroy symbol, and exposes the canonical `new` constructor as
/// an unnamed factory (`Store(...)`), every other constructor as a named
/// factory (`Store.open(...)`), instance methods that pass `_handle` as the
/// implicit leading FFI argument, and statics as `static` methods. Member FFI
/// typedefs and lookups stay at file scope.
fn render_interface(out: &mut String, module: &ModuleBinding, i: &InterfaceBinding) {
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

/// Render one enum. A C-style enum becomes an enhanced Dart `enum`; a rich
/// (algebraic) enum is a value type and becomes a sealed class hierarchy with
/// pack/unpack helpers.
fn render_enum(out: &mut String, e: &EnumBinding) {
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

/// Render one record as a plain Dart value class (final typed fields, a named
/// constructor argument per field), plus its `_pack{Name}`/`_unpack{Name}`
/// buffer helpers. Records declare no C symbols: no destroy, no getters, no
/// builders; instances cross the ABI serialized in value buffers.
fn render_struct(out: &mut String, s: &StructBinding) {
    let class_name = s.name.to_upper_camel_case();
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &s.doc, "");
        w.raw(d);
    }
    w.block(format!("class {class_name} {{"), "}", |w| {
        for f in &s.fields {
            let mut fd = String::new();
            emit_doc(&mut fd, &f.doc, "  ");
            w.raw(fd);
            w.line(format!(
                "final {} {};",
                dart_type(&f.ty),
                f.name.to_lower_camel_case()
            ));
        }
        if !s.fields.is_empty() {
            w.blank();
            let params: Vec<String> = s
                .fields
                .iter()
                .map(|f| {
                    let n = f.name.to_lower_camel_case();
                    if matches!(f.ty, TypeRef::Optional(_)) {
                        format!("this.{n}")
                    } else {
                        format!("required this.{n}")
                    }
                })
                .collect();
            w.line(format!("{class_name}({{{}}});", params.join(", ")));
        }
    });

    // Pack: each field in declaration (wire) order.
    w.blank();
    w.line(format!(
        "void _pack{class_name}(_BufferWriter w, {class_name} v) {{"
    ));
    w.scope(|w| {
        let mut tmp = 0usize;
        for f in &s.fields {
            write_stmts(
                w,
                "w",
                &format!("v.{}", f.name.to_lower_camel_case()),
                &f.ty,
                &mut tmp,
            );
        }
    });
    w.line("}");

    // Unpack: named constructor arguments evaluate in source order, which is
    // the field declaration (wire) order.
    w.blank();
    w.line(format!(
        "{class_name} _unpack{class_name}(_BufferReader r) {{"
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line(format!("return {class_name}();"));
        } else {
            w.line(format!("return {class_name}("));
            w.scope(|w| {
                for f in &s.fields {
                    w.line(format!(
                        "{}: {},",
                        f.name.to_lower_camel_case(),
                        read_expr("r", &f.ty)
                    ));
                }
            });
            w.line(");");
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// The Dart subclass name of one rich-enum variant: `{Enum}{Variant}`.
fn variant_class(base: &str, variant: &str) -> String {
    format!("{base}{}", variant.to_upper_camel_case())
}

/// Render one rich (algebraic) enum as an idiomatic sealed class hierarchy:
/// a sealed base class plus one subclass per variant carrying that variant's
/// fields, and `_pack{Name}`/`_unpack{Name}` helpers encoding the `i32` tag
/// followed by the active variant's fields. Rich enums declare no C symbols;
/// values cross the ABI serialized in value buffers.
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let base = e.name.to_upper_camel_case();
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("sealed class {base} {{"), "}", |w| {
        w.line(format!("const {base}();"));
    });

    for v in &e.variants {
        let cls = variant_class(&base, &v.name);
        w.blank();
        {
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "");
            w.raw(vd);
        }
        if v.fields.is_empty() {
            w.line(format!("class {cls} extends {base} {{}}"));
        } else {
            w.block(format!("class {cls} extends {base} {{"), "}", |w| {
                for f in &v.fields {
                    let mut fd = String::new();
                    emit_doc(&mut fd, &f.doc, "  ");
                    w.raw(fd);
                    w.line(format!(
                        "final {} {};",
                        dart_type(&f.ty),
                        f.name.to_lower_camel_case()
                    ));
                }
                w.blank();
                let params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", f.name.to_lower_camel_case()))
                    .collect();
                w.line(format!("{cls}({});", params.join(", ")));
            });
        }
    }

    // Pack: the i32 tag, then the active variant's fields in order. The
    // sealed base makes the switch exhaustive without a default arm. One
    // temp counter spans all cases: a Dart switch shares a single scope for
    // plain declarations, so names must stay unique across cases.
    w.blank();
    w.line(format!("void _pack{base}(_BufferWriter w, {base} v) {{"));
    w.scope(|w| {
        w.line("switch (v) {");
        w.scope(|w| {
            let mut tmp = 0usize;
            for v in &e.variants {
                let cls = variant_class(&base, &v.name);
                if v.fields.is_empty() {
                    w.line(format!("case {cls}():"));
                    w.scope(|w| {
                        w.line(format!("w.writeInt32({});", v.value));
                    });
                } else {
                    let b = fresh(&mut tmp);
                    w.line(format!("case final {cls} {b}:"));
                    w.scope(|w| {
                        w.line(format!("w.writeInt32({});", v.value));
                        for f in &v.fields {
                            write_stmts(
                                w,
                                "w",
                                &format!("{b}.{}", f.name.to_lower_camel_case()),
                                &f.ty,
                                &mut tmp,
                            );
                        }
                    });
                }
            }
        });
        w.line("}");
    });
    w.line("}");

    // Unpack: constructor arguments evaluate left to right, preserving the
    // wire order of the variant's fields.
    w.blank();
    w.line(format!("{base} _unpack{base}(_BufferReader r) {{"));
    w.scope(|w| {
        w.line("final tag = r.readInt32();");
        w.line("switch (tag) {");
        w.scope(|w| {
            for v in &e.variants {
                let cls = variant_class(&base, &v.name);
                w.line(format!("case {}:", v.value));
                w.scope(|w| {
                    let args: Vec<String> =
                        v.fields.iter().map(|f| read_expr("r", &f.ty)).collect();
                    w.line(format!("return {cls}({});", args.join(", ")));
                });
            }
            w.line("default:");
            w.scope(|w| {
                w.line(format!("_bufferError('unknown {base} tag $tag');"));
            });
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

fn render_function(out: &mut String, module: &ModuleBinding, f: &FnBinding, strip: bool) {
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
    );
    out.push_str(&decl);
}

/// Render one callable: its FFI typedefs and lookups into `lookups` (always
/// top-level) and its Dart wrapper declaration into `decl` (top-level for a
/// free function, spliced into the class body for an interface member).
fn render_callable(
    lookups: &mut String,
    decl: &mut String,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
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

    // Each input parameter expands to its ABI slots (bytes and buffered
    // values fan out to `(ptr, len)`); a bytes or buffered return adds its
    // `out_len` slot; the trailing error slot closes the signature. An
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
        if !matches!(f.shape, CallShape::Iterator(_)) {
            for (n, d) in return_out_slots(ret) {
                native_params.push(n);
                dart_params.push(d);
            }
        }
    }
    native_params.push("Pointer<_WeaveFFIError>".into());
    dart_params.push("Pointer<_WeaveFFIError>".into());

    let (native_ret, dart_ret) = match &f.shape {
        // The iterator launcher returns the opaque iterator handle.
        CallShape::Iterator(_) => ("Pointer<Void>".to_string(), "Pointer<Void>".to_string()),
        _ => match &f.ret {
            Some(ret) => return_ffi(ret),
            None => ("Void".into(), "void".into()),
        },
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
        emit_iterator_body(&mut body, f, c_sym, ib, err);
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
        if matches!(ib.elem, TypeRef::Interface(_)) {
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

/// Emit the statements converting one callback's trampoline slots into the
/// values handed to the user callback, returning the argument expressions.
/// Buffered arguments arrive as borrowed `(ptr, len)` pairs valid only for
/// the dispatch, so they are decoded here, inside the borrow window. Slot
/// names follow the lowered ABI (`{n}` or `{n}_ptr`/`{n}_len`).
fn emit_cb_args(w: &mut CodeWriter, cb: &CallbackBinding) -> Vec<String> {
    let mut args = Vec::new();
    for p in &cb.params {
        let base = p.name.to_lower_camel_case();
        let n0 = p.abi[0].name.to_lower_camel_case();
        if is_buffered(&p.ty) {
            let n1 = p.abi[1].name.to_lower_camel_case();
            w.line(format!("final {base}Data = _copyNativeBytes({n0}, {n1});"));
            w.line(format!("final {base}Reader = _BufferReader({base}Data);"));
            w.line(format!(
                "final {base}Value = {};",
                read_expr(&format!("{base}Reader"), &p.ty)
            ));
            w.line(format!("{base}Reader.expectEnd();"));
            args.push(format!("{base}Value"));
            continue;
        }
        args.push(match &p.ty {
            TypeRef::Enum(name) => format!("{}.fromValue({n0})", dart_class(name)),
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("{n0} == nullptr ? '' : {n0}.toDartString()")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let len = p.abi[1].name.to_lower_camel_case();
                format!("{n0} == nullptr ? <int>[] : {n0}.asTypedList({len}).toList()")
            }
            // Borrowed for the duration of the callback: do not dispose().
            TypeRef::Interface(name) | TypeRef::TypedHandle(name) => {
                format!("{}._({n0})", dart_class(name))
            }
            // Only `Interface?` reaches here (every other optional is
            // buffered): a nullable borrowed object pointer.
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::Interface(name) | TypeRef::TypedHandle(name) => {
                    format!("{n0} == nullptr ? null : {}._({n0})", dart_class(name))
                }
                _ => unreachable!("only optional interfaces stay unbuffered"),
            },
            TypeRef::Named(_) => unreachable!("unresolved type reference"),
            _ => n0,
        });
    }
    args
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
                let call_args = emit_cb_args(w, cb);
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

/// The (native, dart, name) slot triples an async completion callback carries
/// after its `(context, err)` prefix. Bytes and buffered results arrive as
/// borrowed `(result, resultLen)` pairs; interfaces as adopted pointers;
/// everything else by value.
fn async_cb_result_slots(ret: Option<&TypeRef>) -> Vec<(String, String, String)> {
    let Some(ty) = ret else {
        return vec![];
    };
    let pair = |n: &str, d: &str, name: &str| (n.to_string(), d.to_string(), name.to_string());
    if is_buffered(ty) || matches!(ty, TypeRef::Bytes | TypeRef::BorrowedBytes) {
        return vec![
            pair("Pointer<Uint8>", "Pointer<Uint8>", "result"),
            pair("Size", "int", "resultLen"),
        ];
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            vec![pair("Pointer<Utf8>", "Pointer<Utf8>", "result")]
        }
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) | TypeRef::Optional(_) => {
            // Only `Interface?` optionals reach here; the slot is a nullable
            // adopted object pointer.
            vec![pair("Pointer<Void>", "Pointer<Void>", "result")]
        }
        _ => {
            let (n, d) = scalar_ffi(ty);
            vec![pair(n, d, "result")]
        }
    }
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
    let cb_extras = async_cb_result_slots(f.ret.as_ref());
    let cb_native_params: Vec<String> = std::iter::once("Pointer<Void>".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError>".to_string()))
        .chain(cb_extras.iter().map(|(n, _, _)| n.clone()))
        .collect();

    let cb_typedef = format!("_NativeAsyncCb_{c_sym}");
    lookups.push_str(&format!(
        "\ntypedef {cb_typedef} = Void Function({});\n",
        cb_native_params.join(", ")
    ));

    let async_sym = format!("{c_sym}_async");
    let self_slot = if f.has_self {
        vec![("Pointer<Void>".to_string(), "Pointer<Void>".to_string())]
    } else {
        vec![]
    };
    let mut input_ffi: Vec<(String, String)> = self_slot;
    for p in &f.params {
        input_ffi.extend(input_slots(&p.ty));
    }
    if f.cancellable {
        input_ffi.push(("Pointer<Void>".into(), "Pointer<Void>".into()));
    }
    input_ffi.push((
        format!("Pointer<NativeFunction<{cb_typedef}>>"),
        format!("Pointer<NativeFunction<{cb_typedef}>>"),
    ));
    input_ffi.push(("Pointer<Void>".into(), "Pointer<Void>".into()));
    let native_params: Vec<String> = input_ffi.iter().map(|(n, _)| n.clone()).collect();
    let dart_params: Vec<String> = input_ffi.iter().map(|(_, d)| d.clone()).collect();

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

    // Stage every input up front, exactly like the sync path; staged native
    // memory is pinned until the future completes and released in
    // whenComplete (or in the catch when the launch itself throws).
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut stage = CodeWriter::two_space().with_depth(1);
    let mut tmp = 0usize;
    for p in &f.params {
        let args = emit_input(
            &mut stage,
            &p.name.to_lower_camel_case(),
            &p.ty,
            &mut frees,
            &mut tmp,
        );
        call_args.extend(args);
    }
    let staging = stage.finish();
    if f.cancellable {
        call_args.push("nullptr".into());
    }
    call_args.push("callable.nativeFunction".into());
    call_args.push("nullptr".into());

    let cb_param_decls: Vec<String> = std::iter::once("Pointer<Void> context".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError> err".to_string()))
        .chain(cb_extras.iter().map(|(_, d, n)| format!("{d} {n}")))
        .collect();

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
            w.raw(&staging);
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
                        if err.thrown_exception().is_some() {
                            w.line(
                                "final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);",
                            );
                        }
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
                for fr in &frees {
                    w.line(fr);
                }
                w.line("rethrow;");
            });
            w.line("}");
            if frees.is_empty() {
                w.line("return completer.future;");
            } else {
                w.line("return completer.future.whenComplete(() {");
                w.scope(|w| {
                    for fr in &frees {
                        w.line(fr);
                    }
                });
                w.line("});");
            }
        },
    );
    decl.push_str(&w.finish());
}

/// Emit the callback statements that resolve the completer from the result
/// slots. Bytes and buffered results are borrowed for the callback's
/// duration, so they are copied (bytes) or decoded (buffered) here and never
/// freed; an owned interface result is instead adopted by its wrapper class.
fn emit_async_complete(out: &mut String, ty: Option<&TypeRef>, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let Some(ty) = ty else {
        w.line("completer.complete();");
        out.push_str(&w.finish());
        return;
    };
    if is_buffered(ty) {
        // Decode inside the callback: the producer frees the encoding as
        // soon as the callback returns.
        w.line("final resultData = _copyNativeBytes(result, resultLen);");
        w.line("final resultReader = _BufferReader(resultData);");
        w.line(format!("final value = {};", read_expr("resultReader", ty)));
        w.line("resultReader.expectEnd();");
        w.line("completer.complete(value);");
        out.push_str(&w.finish());
        return;
    }
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("completer.complete(_copyNativeBytes(result, resultLen));");
        }
        // Borrowed: copy before the callback returns, never free.
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("completer.complete(result.toDartString());");
        }
        TypeRef::Enum(name) => {
            w.line(format!(
                "completer.complete({}.fromValue(result));",
                dart_class(name)
            ));
        }
        // The callback receives ownership of an object result; the wrapper
        // adopts the pointer and its `dispose()` owns the eventual destroy.
        TypeRef::Interface(name) | TypeRef::TypedHandle(name) => {
            w.line(format!(
                "completer.complete({}._(result));",
                dart_class(name)
            ));
        }
        // Only `Interface?` reaches here: null means none, otherwise adopt.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) | TypeRef::TypedHandle(name) => {
                w.line(format!(
                    "completer.complete(result == nullptr ? null : {}._(result));",
                    dart_class(name)
                ));
            }
            _ => unreachable!("only optional interfaces stay unbuffered"),
        },
        _ => {
            w.line("completer.complete(result);");
        }
    }
    out.push_str(&w.finish());
}

/// Emit pre-call staging for one input (`name`), returning the call-argument
/// expressions it contributes (in ABI order) and appending any cleanup
/// statements to `frees`. A buffered value is encoded into a `_BufferWriter`,
/// staged into native memory, and passed as a borrowed `(ptr, len)` pair the
/// callee never frees.
fn emit_input(
    w: &mut CodeWriter,
    name: &str,
    ty: &TypeRef,
    frees: &mut Vec<String>,
    tmp: &mut usize,
) -> Vec<String> {
    if is_buffered(ty) {
        let writer = format!("{name}Writer");
        let buf = format!("{name}Buf");
        let p = format!("{name}Ptr");
        w.line(format!("final {writer} = _BufferWriter();"));
        write_stmts(w, &writer, name, ty, tmp);
        w.line(format!("final {buf} = {writer}.takeBytes();"));
        w.line(format!("final {p} = _stageBytes({buf});"));
        frees.push(format!("calloc.free({p});"));
        return vec![p, format!("{buf}.length")];
    }
    match ty {
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) => vec![format!("{name}._handle")],
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let p = format!("{name}Ptr");
            w.line(format!("final {p} = {name}.toNativeUtf8();"));
            frees.push(format!("calloc.free({p});"));
            vec![p]
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let p = format!("{name}Ptr");
            w.line(format!(
                "final {p} = {name}.isEmpty ? nullptr : calloc<Uint8>({name}.length);"
            ));
            w.line(format!(
                "for (var i = 0; i < {name}.length; i++) {{ {p}[i] = {name}[i]; }}"
            ));
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p, format!("{name}.length")]
        }
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable borrowed object pointer, null meaning none.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(_) | TypeRef::TypedHandle(_) => {
                vec![format!("{name}?._handle ?? nullptr")]
            }
            _ => unreachable!("only optional interfaces stay unbuffered"),
        },
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        _ => vec![name.to_string()],
    }
}

/// Allocate the out-parameter locals a bytes or buffered return needs before
/// the call, returning the extra call-argument expressions and recording
/// cleanup.
fn emit_return_alloc(w: &mut CodeWriter, ty: &TypeRef, frees: &mut Vec<String>) -> Vec<String> {
    if returns_buffer(ty) {
        w.line("final outLen = calloc<Size>();");
        frees.push("calloc.free(outLen);".into());
        vec!["outLen".into()]
    } else {
        vec![]
    }
}

/// Emit the post-call decode of a return into the wrapper's Dart return
/// value. A buffered return is copied out of the producer's buffer, released
/// with `weaveffi_free_bytes`, and decoded through the buffer reader.
fn emit_return_decode(out: &mut String, ty: &TypeRef, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    if is_buffered(ty) {
        w.line("final n = outLen.value;");
        w.line("final data = _copyNativeBytes(result, n);");
        w.line("if (result != nullptr) _weaveffiFreeBytes(result, n);");
        w.line("final reader = _BufferReader(data);");
        w.line(format!("final value = {};", read_expr("reader", ty)));
        w.line("reader.expectEnd();");
        w.line("return value;");
        out.push_str(&w.finish());
        return;
    }
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("final n = outLen.value;");
            w.line("if (result == nullptr) return <int>[];");
            w.line("final bytes = List<int>.generate(n, (i) => result[i]);");
            // Copy first, then release the producer's buffer.
            w.line("_weaveffiFreeBytes(result, n);");
            w.line("return bytes;");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("final value = result.toDartString();");
            w.line("_weaveffiFreeString(result);");
            w.line("return value;");
        }
        TypeRef::Enum(name) => {
            w.line(format!("return {}.fromValue(result);", dart_class(name)));
        }
        TypeRef::Interface(name) | TypeRef::TypedHandle(name) => {
            w.line(format!("return {}._(result);", dart_class(name)));
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) | TypeRef::TypedHandle(name) => {
                w.line("if (result == nullptr) return null;");
                w.line(format!("return {}._(result);", dart_class(name)));
            }
            _ => unreachable!("only optional interfaces stay unbuffered"),
        },
        _ => {
            w.line("return result;");
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
    let mut stage = CodeWriter::two_space().with_depth(1);
    let mut tmp = 0usize;
    for p in &f.params {
        let args = emit_input(
            &mut stage,
            &p.name.to_lower_camel_case(),
            &p.ty,
            &mut frees,
            &mut tmp,
        );
        call_args.extend(args);
    }
    if let Some(ret) = &f.ret {
        call_args.extend(emit_return_alloc(&mut stage, ret, &mut frees));
    }
    let staging = stage.finish();
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    let args = call_args.join(", ");
    let void_call = f.ret.is_none();
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

/// The dart:ffi pointee type of an iterator's `out_item` slot, plus whether
/// the element also carries a `size_t* out_len` slot (bytes and every
/// buffered element do).
fn iter_item_slot(elem: &TypeRef) -> (String, bool) {
    if is_buffered(elem) || matches!(elem, TypeRef::Bytes | TypeRef::BorrowedBytes) {
        return ("Pointer<Uint8>".into(), true);
    }
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("Pointer<Utf8>".into(), false),
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) => ("Pointer<Void>".into(), false),
        _ => (scalar_ffi(elem).0.to_string(), false),
    }
}

/// Convert a single native by-value element (`expr`) into its Dart
/// representation: enums map through `fromValue`, interface elements are
/// adopted by their wrapper class, scalars pass through.
fn direct_elem_read(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::Enum(n) => format!("{}.fromValue({expr})", dart_class(n)),
        TypeRef::Interface(n) | TypeRef::TypedHandle(n) => {
            format!("{}._({expr})", dart_class(n))
        }
        _ => expr.to_string(),
    }
}

/// Bind the element `next`/`destroy` symbols of an iterator-returning
/// function, plus a `NativeFinalizer` over the destroy symbol. The finalizer
/// is the disposal backstop for abandoned iterations: Dart runs a `sync*`
/// body only inside `moveNext`, so a consumer that stops pulling (a broken
/// `for` loop, `first`, `take`) never resumes the generator and its `finally`
/// block never runs; the finalizer reclaims the native handle when the
/// suspended frame is collected instead.
fn emit_iter_lookups(out: &mut String, ib: &IteratorBinding) {
    let (pointee, has_len) = iter_item_slot(&ib.elem);
    let mut params = vec!["Pointer<Void>".to_string(), format!("Pointer<{pointee}>")];
    if has_len {
        params.push("Pointer<Size>".into());
    }
    params.push("Pointer<_WeaveFFIError>".into());
    let joined = params.join(", ");
    emit_typedef_and_lookup(out, &ib.next.symbol, &joined, &joined, "Int32", "int");
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
/// copying or decoding (strings through `weaveffi_free_string`; bytes and
/// buffered elements through `weaveffi_free_bytes`; interface elements are
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
) {
    let free_plan = elem_free(&ib.elem);
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut stage = CodeWriter::two_space().with_depth(1);
    let mut tmp = 0usize;
    for p in &f.params {
        let args = emit_input(
            &mut stage,
            &p.name.to_lower_camel_case(),
            &p.ty,
            &mut frees,
            &mut tmp,
        );
        call_args.extend(args);
    }
    let staging = stage.finish();
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    let elem = &ib.elem;
    let (pointee, has_len) = iter_item_slot(elem);
    let next_var = ib.next.symbol.to_lower_camel_case();
    let destroy_var = ib.destroy_symbol.to_lower_camel_case();
    let next_args = if has_len {
        "iter, outItem, outLen, err"
    } else {
        "iter, outItem, err"
    };

    let mut w = CodeWriter::two_space().with_depth(1);
    w.raw(staging);
    w.line("final err = calloc<_WeaveFFIError>();");
    w.line(format!("final outItem = calloc<{pointee}>();"));
    if has_len {
        w.line("final outLen = calloc<Size>();");
    }
    w.line("Pointer<Void> iter = nullptr;");
    w.line("final anchor = _IteratorLifetime();");
    w.line("try {");
    w.scope(|w| {
        w.line(format!("iter = _{var}({});", call_args.join(", ")));
        w.line(err.check_stmt());
        w.line(format!(
            "_{destroy_var}Finalizer.attach(anchor, iter, detach: anchor);"
        ));
        w.line(format!("while (_{next_var}({next_args}) != 0) {{"));
        w.scope(|w| {
            w.line(err.check_stmt());
            match free_plan {
                ElemFree::String => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final item = itemPtr.toDartString();");
                    w.line("_weaveffiFreeString(itemPtr);");
                    w.line("yield item;");
                }
                // Bytes and buffered elements: copy or decode, then release
                // the producer's buffer with weaveffi_free_bytes.
                ElemFree::Bytes => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final itemLen = outLen.value;");
                    if is_buffered(elem) {
                        w.line("final itemData = _copyNativeBytes(itemPtr, itemLen);");
                        w.line("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);");
                        w.line("final itemReader = _BufferReader(itemData);");
                        w.line(format!("final item = {};", read_expr("itemReader", elem)));
                        w.line("itemReader.expectEnd();");
                        w.line("yield item;");
                    } else {
                        w.line("final item = _copyNativeBytes(itemPtr, itemLen);");
                        w.line("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);");
                        w.line("yield item;");
                    }
                }
                // By-value element (or an adopted interface handle).
                ElemFree::None => {
                    w.line(format!(
                        "yield {};",
                        direct_elem_read("outItem.value", elem)
                    ));
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
        if has_len {
            w.line("calloc.free(outLen);");
        }
        w.line("calloc.free(outItem);");
        for fr in &frees {
            w.line(fr);
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.6.0".into(),
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

    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
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
    fn package_bundles_native_and_rewrites_loader() {
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![simple_module(vec![func("ping", vec![], None)])]);
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

    /// Buffered types occupy one `(Pointer<Uint8>, Size)` slot pair at the
    /// FFI layer, no matter how deeply they nest; direct types keep their
    /// dedicated slots.
    #[test]
    fn input_slots_mapping() {
        let pair = |n: &str, d: &str| (n.to_string(), d.to_string());
        let buffer = vec![
            pair("Pointer<Uint8>", "Pointer<Uint8>"),
            pair("Size", "int"),
        ];
        assert_eq!(input_slots(&TypeRef::Record("C".into())), buffer);
        assert_eq!(input_slots(&TypeRef::RichEnum("S".into())), buffer);
        assert_eq!(input_slots(&TypeRef::List(Box::new(TypeRef::I32))), buffer);
        assert_eq!(
            input_slots(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            buffer
        );
        assert_eq!(
            input_slots(&TypeRef::Optional(Box::new(TypeRef::I32))),
            buffer
        );
        // Bytes share the (ptr, len) fan-out but are not a value buffer.
        assert_eq!(input_slots(&TypeRef::Bytes), buffer);
        assert_eq!(input_slots(&TypeRef::I32), vec![pair("Int32", "int")]);
        assert_eq!(input_slots(&TypeRef::Bool), vec![pair("Bool", "bool")]);
        assert_eq!(
            input_slots(&TypeRef::StringUtf8),
            vec![pair("Pointer<Utf8>", "Pointer<Utf8>")]
        );
        assert_eq!(
            input_slots(&TypeRef::Interface("Store".into())),
            vec![pair("Pointer<Void>", "Pointer<Void>")]
        );
        // The one optional exception: a nullable interface pointer.
        assert_eq!(
            input_slots(&TypeRef::Optional(Box::new(TypeRef::Interface(
                "Store".into()
            )))),
            vec![pair("Pointer<Void>", "Pointer<Void>")]
        );
    }

    /// Buffered and bytes returns come back as `Pointer<Uint8>` plus a
    /// trailing `Pointer<Size>` out-slot; everything else keeps its direct
    /// return slot with no out-params.
    #[test]
    fn return_slots_mapping() {
        for ty in [
            TypeRef::Record("C".into()),
            TypeRef::RichEnum("S".into()),
            TypeRef::List(Box::new(TypeRef::Record("C".into()))),
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            TypeRef::Optional(Box::new(TypeRef::I64)),
            TypeRef::Bytes,
        ] {
            assert_eq!(
                return_ffi(&ty),
                ("Pointer<Uint8>".to_string(), "Pointer<Uint8>".to_string()),
                "{ty:?}"
            );
            assert_eq!(
                return_out_slots(&ty),
                vec![("Pointer<Size>".to_string(), "Pointer<Size>".to_string())],
                "{ty:?}"
            );
        }
        assert_eq!(
            return_ffi(&TypeRef::StringUtf8),
            ("Pointer<Utf8>".to_string(), "Pointer<Utf8>".to_string())
        );
        assert!(return_out_slots(&TypeRef::StringUtf8).is_empty());
        assert_eq!(
            return_ffi(&TypeRef::Optional(Box::new(TypeRef::Interface(
                "Store".into()
            )))),
            ("Pointer<Void>".to_string(), "Pointer<Void>".to_string())
        );
        assert!(return_out_slots(&TypeRef::I32).is_empty());
    }

    #[test]
    fn generate_dart_basic() {
        let api = make_api(vec![simple_module(vec![func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )])]);

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

    /// The library always ships the private value-buffer runtime: a
    /// little-endian writer/reader pair with truncation, flag-byte, and
    /// trailing-bytes validation, plus the staging/copy helpers.
    #[test]
    fn emits_value_buffer_runtime() {
        let api = make_api(vec![simple_module(vec![func("ping", vec![], None)])]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("final class _BufferWriter {"),
            "missing writer: {dart}"
        );
        assert!(
            dart.contains("final class _BufferReader {"),
            "missing reader: {dart}"
        );
        assert!(
            dart.contains("Endian.little"),
            "buffers must be little-endian: {dart}"
        );
        assert!(
            dart.contains("import 'dart:typed_data';") && dart.contains("import 'dart:convert';"),
            "missing runtime imports: {dart}"
        );
        // Decoders reject truncation, hostile lengths, bad flag bytes, and
        // trailing garbage.
        assert!(
            dart.contains("if (_remaining < n) _bufferError(context);"),
            "missing truncation check: {dart}"
        );
        assert!(
            dart.contains("length prefix exceeds remaining buffer"),
            "missing length validation: {dart}"
        );
        assert!(
            dart.contains("bool byte out of range")
                && dart.contains("option flag byte out of range"),
            "missing flag validation: {dart}"
        );
        assert!(
            dart.contains("trailing bytes after value"),
            "missing expectEnd validation: {dart}"
        );
        assert!(
            dart.contains("Pointer<Uint8> _stageBytes(Uint8List bytes)")
                && dart.contains("Uint8List _copyNativeBytes(Pointer<Uint8> ptr, int len)"),
            "missing staging helpers: {dart}"
        );
    }

    /// The error struct mirrors the C `weaveffi_error`, including the
    /// structured payload slots.
    #[test]
    fn error_struct_has_payload_slots() {
        let api = make_api(vec![simple_module(vec![func("ping", vec![], None)])]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("external Pointer<Uint8> payloadPtr;"),
            "missing payload pointer: {dart}"
        );
        assert!(
            dart.contains("@Size()\n  external int payloadLen;"),
            "missing payload length: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_structs() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                fields: vec![
                    field("id", TypeRef::I64),
                    field("first_name", TypeRef::StringUtf8),
                    field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
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

        // A record is a plain Dart value class with final typed fields.
        assert!(dart.contains("class Contact {"), "missing class: {dart}");
        assert!(dart.contains("final int id;"), "missing id field: {dart}");
        assert!(
            dart.contains("final String firstName;"),
            "missing firstName field: {dart}"
        );
        assert!(
            dart.contains("final String? email;"),
            "missing optional email field: {dart}"
        );
        assert!(
            dart.contains("Contact({required this.id, required this.firstName, this.email});"),
            "missing value constructor: {dart}"
        );
        // No C symbols exist for a record: no handle, no dispose, no getters,
        // no builders.
        assert!(
            !dart.contains("Contact._("),
            "record must not wrap a native handle: {dart}"
        );
        assert!(
            !dart.contains("weaveffi_contacts_Contact_destroy")
                && !dart.contains("weaveffi_contacts_Contact_get_"),
            "record must not bind C symbols: {dart}"
        );
        assert!(
            !dart.contains("ContactBuilder"),
            "builders are gone: {dart}"
        );
        // One pack and one unpack helper, fields in declaration (wire) order.
        assert!(
            dart.contains("void _packContact(_BufferWriter w, Contact v) {"),
            "missing pack helper: {dart}"
        );
        assert!(
            dart.contains("w.writeInt64(v.id);"),
            "missing i64 field write: {dart}"
        );
        assert!(
            dart.contains("w.writeString(v.firstName);"),
            "missing string field write: {dart}"
        );
        assert!(
            dart.contains("w.writeOptionFlag(false);") && dart.contains("w.writeOptionFlag(true);"),
            "missing optional flag writes: {dart}"
        );
        assert!(
            dart.contains("Contact _unpackContact(_BufferReader r) {"),
            "missing unpack helper: {dart}"
        );
        assert!(
            dart.contains("id: r.readInt64(),")
                && dart.contains("firstName: r.readString(),")
                && dart.contains("email: (r.readOptionFlag() ? r.readString() : null),"),
            "missing field reads in wire order: {dart}"
        );
    }

    /// A record field of optional type is not `required`; every other field
    /// is.
    #[test]
    fn record_constructor_requires_non_optional_fields() {
        let api = make_api(vec![Module {
            name: "geo".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: None,
                fields: vec![
                    field("x", TypeRef::F64),
                    field("label", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
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
        assert!(
            dart.contains("Point({required this.x, this.label});"),
            "optional fields must not be required: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_enums() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![func(
                "mix",
                vec![param("color", TypeRef::Enum("Color".into()))],
                Some(TypeRef::Enum("Color".into())),
            )],
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
        let api = make_api(vec![simple_module(vec![func("reset", vec![], None)])]);

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
            functions: vec![func(
                "echo",
                vec![param("msg", TypeRef::StringUtf8)],
                Some(TypeRef::StringUtf8),
            )],
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
        let api = make_api(vec![simple_module(vec![func(
            "is_valid",
            vec![param("flag", TypeRef::Bool)],
            Some(TypeRef::Bool),
        )])]);

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
            r#async: true,
            ..func(
                "fetch_data",
                vec![param("id", TypeRef::I32)],
                Some(TypeRef::StringUtf8),
            )
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
            r#async: true,
            ..func(
                "fetch_data",
                vec![param("id", TypeRef::I32)],
                Some(TypeRef::StringUtf8),
            )
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
    fn record_return_decodes_buffer() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![func(
                "get_contact",
                vec![param("id", TypeRef::Handle)],
                Some(TypeRef::Record("Contact".into())),
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
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
            dart.contains("Contact getContact(int id)"),
            "missing signature: {dart}"
        );
        // The buffered return is `Pointer<Uint8>` plus an `out_len` slot.
        assert!(
            dart.contains("Pointer<Uint8> Function(Int64, Pointer<Size>, Pointer<_WeaveFFIError>)"),
            "missing buffered return typedef: {dart}"
        );
        assert!(
            dart.contains("final outLen = calloc<Size>();"),
            "missing out_len alloc: {dart}"
        );
        // Copy, free the producer's buffer, decode, and reject trailing bytes.
        assert!(
            dart.contains("final data = _copyNativeBytes(result, n);"),
            "missing copy: {dart}"
        );
        assert!(
            dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
            "buffered return must be freed after copying: {dart}"
        );
        assert!(
            dart.contains("final value = _unpackContact(reader);"),
            "missing decode: {dart}"
        );
        assert!(
            dart.contains("reader.expectEnd();"),
            "missing trailing-bytes check: {dart}"
        );
    }

    /// A buffered parameter is encoded, staged into native memory, passed as
    /// a borrowed (ptr, len) pair, and freed by the caller afterwards.
    #[test]
    fn record_param_staged_and_freed() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![func(
                "save",
                vec![param("contact", TypeRef::Record("Contact".into()))],
                None,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
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
            dart.contains("final contactWriter = _BufferWriter();")
                && dart.contains("_packContact(contactWriter, contact);"),
            "missing param encode: {dart}"
        );
        assert!(
            dart.contains("final contactBuf = contactWriter.takeBytes();")
                && dart.contains("final contactPtr = _stageBytes(contactBuf);"),
            "missing native staging: {dart}"
        );
        assert!(
            dart.contains("_weaveffiContactsSave(contactPtr, contactBuf.length, err);"),
            "missing (ptr, len) call: {dart}"
        );
        assert!(
            dart.contains("calloc.free(contactPtr);"),
            "staged buffer must be freed by the caller: {dart}"
        );
        // The callee borrows the encoding; the wrapper never routes a
        // parameter through the runtime frees.
        assert!(
            !dart.contains("_weaveffiFreeBytes(contactPtr"),
            "borrowed param must not be runtime-freed: {dart}"
        );
    }

    #[test]
    fn handle_uses_int64() {
        let api = make_api(vec![simple_module(vec![func(
            "create",
            vec![],
            Some(TypeRef::Handle),
        )])]);

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
            functions: vec![func(
                "find_user",
                vec![param("id", TypeRef::I64)],
                Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            )],
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
        // An optional is buffered: a flag byte, then the value when present.
        assert!(
            dart.contains("final value = (reader.readOptionFlag() ? reader.readString() : null);"),
            "missing optional decode: {dart}"
        );
        assert!(
            dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
            "optional return buffer must be freed: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_lists() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![func(
                "get_scores",
                vec![param("items", TypeRef::List(Box::new(TypeRef::I32)))],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            )],
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
        // The list input is one serialized buffer: count then elements.
        assert!(
            dart.contains("itemsWriter.writeLength(t0.length);")
                && dart.contains("itemsWriter.writeInt32(t1);"),
            "missing list encode: {dart}"
        );
        assert!(
            dart.contains("_weaveffiDataGetScores(itemsPtr, itemsBuf.length, outLen, err)"),
            "missing (ptr, len) call with out_len: {dart}"
        );
        // The list return decodes count then elements from one buffer.
        assert!(
            dart.contains(
                "final value = List<String>.generate(reader.readLength(), (_) => reader.readString());"
            ),
            "missing list decode: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_maps() {
        let api = make_api(vec![Module {
            name: "cache".into(),
            functions: vec![func(
                "get_entries",
                vec![],
                Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
            )],
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
        // A map is one buffer: count then alternating key, value.
        assert!(
            dart.contains(
                "<String, int>{ for (var i = reader.readLength(); i > 0; i--) reader.readString(): reader.readInt32() }"
            ),
            "missing map decode: {dart}"
        );
        assert!(
            !dart.contains("outKeys") && !dart.contains("outValues"),
            "parallel key/value arrays are gone: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_typed_handle() {
        let api = make_api(vec![Module {
            name: "sessions".into(),
            functions: vec![
                func(
                    "create_session",
                    vec![param("name", TypeRef::StringUtf8)],
                    Some(TypeRef::TypedHandle("Session".into())),
                ),
                func(
                    "close_session",
                    vec![param("session", TypeRef::TypedHandle("Session".into()))],
                    None,
                ),
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
                func(
                    "create_contact",
                    vec![
                        param("first_name", TypeRef::StringUtf8),
                        param("last_name", TypeRef::StringUtf8),
                        param("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                        param("contact_type", TypeRef::Enum("ContactType".into())),
                    ],
                    Some(TypeRef::Handle),
                ),
                func(
                    "get_contact",
                    vec![param("id", TypeRef::Handle)],
                    Some(TypeRef::Record("Contact".into())),
                ),
                func(
                    "list_contacts",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                ),
                func(
                    "delete_contact",
                    vec![param("id", TypeRef::Handle)],
                    Some(TypeRef::Bool),
                ),
                func("count_contacts", vec![], Some(TypeRef::I32)),
            ],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                fields: vec![
                    field("id", TypeRef::I64),
                    field("first_name", TypeRef::StringUtf8),
                    field("last_name", TypeRef::StringUtf8),
                    field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    field("contact_type", TypeRef::Enum("ContactType".into())),
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
        assert!(
            dart.contains("final int id;")
                && dart.contains("final String firstName;")
                && dart.contains("final String? email;")
                && dart.contains("final ContactType contactType;"),
            "missing typed final fields: {dart}"
        );
        // The enum field crosses the buffer as its i32 discriminant.
        assert!(
            dart.contains("w.writeInt32(v.contactType.value);")
                && dart.contains("contactType: ContactType.fromValue(r.readInt32()),"),
            "missing enum field encode/decode: {dart}"
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
            dart.contains("(_) => _unpackContact(reader)"),
            "list of records must decode elements: {dart}"
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
                fields: vec![field("name", TypeRef::StringUtf8)],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            functions: vec![func(
                "find_contact",
                vec![param("name", TypeRef::StringUtf8)],
                Some(TypeRef::Record("Contact".into())),
            )],
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
        let decode = fn_body
            .find("_unpackContact(reader)")
            .expect("decode in findContact");
        assert!(
            err_check < decode,
            "error must be checked before decoding the return buffer: {dart}"
        );
    }

    #[test]
    fn dart_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![func(
                "find_contact",
                vec![param("id", TypeRef::I32)],
                Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
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
            dart.contains("Contact? findContact(int id)"),
            "missing optional record signature: {dart}"
        );
        // The option is a flag byte inside the buffer, not a null pointer.
        assert!(
            dart.contains(
                "final value = (reader.readOptionFlag() ? _unpackContact(reader) : null);"
            ),
            "optional record must decode the flag then the value: {dart}"
        );
    }

    fn doc_api() -> Api {
        make_api(vec![Module {
            name: "docs".into(),
            functions: vec![Function {
                doc: Some("Performs a thing.".into()),
                ..func(
                    "do_thing",
                    vec![param("x", TypeRef::I32)],
                    Some(TypeRef::I32),
                )
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
        assert!(
            dart.contains("/// Stable id\n  final int id;"),
            "field doc should sit on the final field: {dart}"
        );
    }

    /// A rich (algebraic) enum mirroring `samples/shapes`: a unit variant, an
    /// f64 payload, two f32 payloads, and a (string, u8) payload, plus a plain
    /// sibling enum and functions that take/return the rich enum by value.
    fn rich_enum_api() -> Api {
        make_api(vec![Module {
            name: "shapes".into(),
            functions: vec![
                func(
                    "describe",
                    vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                    Some(TypeRef::StringUtf8),
                ),
                func(
                    "scale",
                    vec![
                        param("shape", TypeRef::RichEnum("Shape".into())),
                        param("factor", TypeRef::F64),
                    ],
                    Some(TypeRef::RichEnum("Shape".into())),
                ),
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn rich_enum_is_sealed_hierarchy() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // The rich enum must NOT be a plain Dart `enum`...
        assert!(
            !dart.contains("enum Shape {"),
            "rich enum must not render as a plain enum: {dart}"
        );
        // ...but a sealed base class with one subclass per variant.
        assert!(
            dart.contains("sealed class Shape {"),
            "missing sealed base: {dart}"
        );
        assert!(
            dart.contains("class ShapeEmpty extends Shape {}"),
            "missing unit variant subclass: {dart}"
        );
        assert!(
            dart.contains("class ShapeCircle extends Shape {"),
            "missing circle subclass: {dart}"
        );
        assert!(
            dart.contains("final double radius;") && dart.contains("ShapeCircle(this.radius);"),
            "variant fields must be final constructor fields: {dart}"
        );
        assert!(
            dart.contains("ShapeLabeled(this.label, this.count);"),
            "multi-field variant constructor: {dart}"
        );
        // Rich enums declare no C symbols: no handle, no dispose, no
        // per-variant constructors or getters.
        assert!(
            !dart.contains("Shape._(") && !dart.contains("weaveffi_shapes_Shape_"),
            "rich enum must not bind C symbols: {dart}"
        );
        // Carries the per-variant field doc onto the final field.
        assert!(
            dart.contains("/// Radius in points"),
            "variant field doc should be emitted: {dart}"
        );
        // A plain sibling enum still renders as a plain Dart enum.
        assert!(
            dart.contains("enum Channel {"),
            "plain sibling enum should still render as an enum: {dart}"
        );
    }

    #[test]
    fn rich_enum_pack_writes_tag_then_fields() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("void _packShape(_BufferWriter w, Shape v) {"),
            "missing pack helper: {dart}"
        );
        assert!(
            dart.contains("case ShapeEmpty():") && dart.contains("w.writeInt32(0);"),
            "unit variant must write only its tag: {dart}"
        );
        assert!(
            dart.contains("case final ShapeCircle t0:")
                && dart.contains("w.writeInt32(1);")
                && dart.contains("w.writeFloat64(t0.radius);"),
            "circle must write tag then f64 radius: {dart}"
        );
        assert!(
            dart.contains("w.writeFloat32(t1.width);")
                && dart.contains("w.writeFloat32(t1.height);"),
            "rectangle must write both f32 fields in order: {dart}"
        );
        assert!(
            dart.contains("w.writeString(t2.label);") && dart.contains("w.writeUint8(t2.count);"),
            "labeled must write string then u8: {dart}"
        );
    }

    #[test]
    fn rich_enum_unpack_switches_on_tag() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Shape _unpackShape(_BufferReader r) {"),
            "missing unpack helper: {dart}"
        );
        assert!(
            dart.contains("final tag = r.readInt32();"),
            "missing tag read: {dart}"
        );
        assert!(
            dart.contains("return ShapeEmpty();"),
            "missing unit variant arm: {dart}"
        );
        assert!(
            dart.contains("return ShapeCircle(r.readFloat64());"),
            "missing circle arm: {dart}"
        );
        assert!(
            dart.contains("return ShapeRectangle(r.readFloat32(), r.readFloat32());"),
            "missing rectangle arm: {dart}"
        );
        assert!(
            dart.contains("return ShapeLabeled(r.readString(), r.readUint8());"),
            "missing labeled arm: {dart}"
        );
        // An unknown tag is a contract violation, not a silent default.
        assert!(
            dart.contains("_bufferError('unknown Shape tag $tag');"),
            "missing unknown-tag rejection: {dart}"
        );
    }

    #[test]
    fn rich_enum_functions_marshal_buffers() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("String describe(Shape shape)"),
            "missing describe signature: {dart}"
        );
        assert!(
            dart.contains("Shape scale(Shape shape, double factor)"),
            "missing scale signature: {dart}"
        );
        // A rich-enum argument is encoded and staged as a (ptr, len) pair...
        assert!(
            dart.contains("_packShape(shapeWriter, shape);")
                && dart.contains("final shapePtr = _stageBytes(shapeBuf);"),
            "rich-enum argument must be encoded and staged: {dart}"
        );
        // ...and a rich-enum return decodes then frees the buffer.
        assert!(
            dart.contains("final value = _unpackShape(reader);"),
            "rich-enum return must decode: {dart}"
        );
        assert!(
            dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
            "rich-enum return buffer must be freed: {dart}"
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
                throws,
                r#async: is_async,
                ..func(name, params, returns)
            }
        }
        make_api(vec![Module {
            name: "kv".into(),
            functions: vec![f(
                "inspect",
                vec![param("store", TypeRef::Interface("Store".into()))],
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
                    f(
                        "new",
                        vec![param("capacity", TypeRef::I64)],
                        None,
                        false,
                        false,
                    ),
                    f(
                        "open",
                        vec![param("path", TypeRef::StringUtf8)],
                        None,
                        true,
                        false,
                    ),
                ],
                methods: vec![
                    f(
                        "put",
                        vec![
                            param("key", TypeRef::StringUtf8),
                            param("value", TypeRef::StringUtf8),
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
                        fields: vec![],
                    },
                    ErrorCode {
                        name: "IoError".into(),
                        code: 1004,
                        message: "I/O failure".into(),
                        doc: None,
                        fields: vec![],
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
        // The mapper covers each code, receives the payload buffer, and falls
        // back to the generic exception.
        assert!(
            dart.contains(
                "WeaveFFIException _mapKvException(int code, String message, Uint8List payload) {"
            ),
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
        // The per-domain check helper copies the payload before clearing.
        assert!(
            dart.contains("void _checkKvException(Pointer<_WeaveFFIError> err) {")
                && dart.contains(
                    "final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);"
                )
                && dart.contains("throw _mapKvException(code, msg, payload);"),
            "missing domain check helper: {dart}"
        );
    }

    /// A code that declares payload fields decodes them from the error's
    /// payload buffer and exposes them as typed properties on the exception.
    #[test]
    fn error_payload_fields_decode_onto_exception() {
        use weaveffi_ir::ir::{ErrorCode, ErrorDomain};
        let api = make_api(vec![Module {
            name: "kv".into(),
            functions: vec![Function {
                throws: true,
                ..func(
                    "get",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::I64),
                )
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: Some(ErrorDomain {
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
                        name: "IoError".into(),
                        code: 1004,
                        message: "I/O failure".into(),
                        doc: None,
                        fields: vec![],
                    },
                ],
            }),
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi", "kv.yml");
        // The exception carries the decoded payload as final typed fields.
        assert!(
            dart.contains("class KeyNotFoundException extends KvException {")
                && dart.contains("final String key;")
                && dart.contains("final int attempts;"),
            "missing payload fields on the exception: {dart}"
        );
        assert!(
            dart.contains(
                "KeyNotFoundException(this.key, this.attempts, [String message = 'key not found']) : super(1001, message);"
            ),
            "missing payload-aware constructor: {dart}"
        );
        // The mapper decodes the payload in declaration (wire) order and
        // rejects trailing bytes.
        assert!(
            dart.contains("final r = _BufferReader(payload);")
                && dart.contains("final v0 = r.readString();")
                && dart.contains("final v1 = r.readInt32();")
                && dart.contains("r.expectEnd();")
                && dart.contains("return KeyNotFoundException(v0, v1, message);"),
            "mapper must decode payload fields in order: {dart}"
        );
        // A code without fields still maps directly.
        assert!(
            dart.contains("return IoException(message);"),
            "plain code must map without payload decoding: {dart}"
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
        // The typed completion copies the payload inside the borrow window
        // and completes with the mapped domain exception.
        assert!(
            dart.contains("completer.completeError(_mapKvException(code, msg, payload));"),
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
            body.contains(
                "while (_weaveffiKvStoreListKeysIteratorNext(iter, outItem, err) != 0) {"
            ),
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

    /// A free function returning `iter<record>` decodes each producer buffer
    /// then frees it with `weaveffi_free_bytes` (`ElemFree::Bytes`), and its
    /// `_next` slot carries the extra `out_len` pointer.
    #[test]
    fn iterator_of_records_decodes_and_frees_elements() {
        let api = make_api(vec![Module {
            name: "kv".into(),
            functions: vec![Function {
                doc: Some("Streams every entry.".into()),
                ..func(
                    "entries",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
                )
            }],
            structs: vec![StructDef {
                name: "Entry".into(),
                doc: None,
                fields: vec![field("key", TypeRef::StringUtf8)],
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
        // The `_next` typedef carries `out_item` plus `out_len`.
        assert!(
            dart.contains(
                "Pointer<Void>, Pointer<Pointer<Uint8>>, Pointer<Size>, Pointer<_WeaveFFIError>"
            ),
            "missing buffered next slots: {dart}"
        );
        assert!(
            dart.contains("final outLen = calloc<Size>();"),
            "missing out_len alloc: {dart}"
        );
        // Each element is copied, freed with weaveffi_free_bytes, and decoded.
        assert!(
            dart.contains("final itemData = _copyNativeBytes(itemPtr, itemLen);")
                && dart.contains("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);")
                && dart.contains("final item = _unpackEntry(itemReader);")
                && dart.contains("itemReader.expectEnd();"),
            "record elements must be decoded then freed: {dart}"
        );
        // Non-throwing: launch and next errors trap via the generic check.
        let body = &dart[dart.find("Iterable<Entry> entries()").expect("body")..];
        assert!(
            body.contains("_checkError(err);"),
            "trap-strategy iterator must use the generic check: {dart}"
        );
        // The generated doc states the streaming contract.
        assert!(
            dart.contains("/// Returns a lazy [Iterable]:"),
            "missing streaming doc: {dart}"
        );
        // Record elements are plain values now; no dispose note applies.
        assert!(
            !dart.contains("/// Each yielded element is owned by the caller:"),
            "record elements carry no dispose obligation: {dart}"
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
                    throws: true,
                    ..func(
                        "div",
                        vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                        Some(TypeRef::I32),
                    )
                },
                func(
                    "add",
                    vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                    Some(TypeRef::I32),
                ),
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
                    fields: vec![],
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
            dart.contains("void _packContact(_BufferWriter w, Contact v) {")
                && dart.contains("Contact _unpackContact(_BufferReader r) {"),
            "missing Contact buffer helpers: {dart}"
        );
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
        // Records declare no C symbols in the new ABI.
        assert!(
            !dart.contains("weaveffi_contacts_Contact_"),
            "record C symbols must be gone: {dart}"
        );
    }

    /// One-function module helper for the ownership-audit tests below.
    fn returning(name: &str, returns: TypeRef) -> Api {
        make_api(vec![simple_module(vec![func(name, vec![], Some(returns))])])
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
    fn string_list_return_decodes_one_buffer() {
        let dart = render_dart_module(
            &returning("names", TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "weaveffi",
            "weaveffi.yml",
        );
        // One producer buffer holding count + length-prefixed strings; no
        // per-element C strings exist any more.
        assert!(
            dart.contains(
                "final value = List<String>.generate(reader.readLength(), (_) => reader.readString());"
            ),
            "missing element decode: {dart}"
        );
        assert!(
            dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
            "list buffer must be freed once: {dart}"
        );
        assert!(
            !dart.contains("_weaveffiFreeString(arr[i]);"),
            "no per-element string frees remain: {dart}"
        );
    }

    #[test]
    fn map_return_decodes_one_buffer() {
        let dart = render_dart_module(
            &returning(
                "tally",
                TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            ),
            "weaveffi",
            "weaveffi.yml",
        );
        assert!(
            dart.contains(
                "<String, int>{ for (var i = reader.readLength(); i > 0; i--) reader.readString(): reader.readInt32() }"
            ),
            "missing map decode: {dart}"
        );
        assert!(
            dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
            "map buffer must be freed once: {dart}"
        );
    }

    #[test]
    fn optional_scalar_return_decodes_flag() {
        let dart = render_dart_module(
            &returning("level", TypeRef::Optional(Box::new(TypeRef::I64))),
            "weaveffi",
            "weaveffi.yml",
        );
        assert!(
            dart.contains("final value = (reader.readOptionFlag() ? reader.readInt64() : null);"),
            "boxed optionals are gone; the flag byte decides presence: {dart}"
        );
        assert!(
            dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
            "optional return buffer must be freed: {dart}"
        );
    }

    /// Async result buffers are borrowed for the callback's duration: the
    /// wrapper decodes them inside the callback and never frees them.
    #[test]
    fn async_buffer_results_decode_and_never_free() {
        let api = make_api(vec![simple_module(vec![
            Function {
                r#async: true,
                ..func(
                    "fetch_names",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                )
            },
            Function {
                r#async: true,
                ..func("fetch_blob", vec![], Some(TypeRef::Bytes))
            },
        ])]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("Future<List<String>> fetchNames()"),
            "missing async list wrapper: {dart}"
        );
        // The borrowed (ptr, len) pair is copied and decoded in the callback.
        assert!(
            dart.contains("final resultData = _copyNativeBytes(result, resultLen);")
                && dart.contains(
                    "final value = List<String>.generate(resultReader.readLength(), (_) => resultReader.readString());"
                ),
            "async buffered result must be decoded inside the callback: {dart}"
        );
        assert!(
            dart.contains("completer.complete(_copyNativeBytes(result, resultLen));"),
            "async bytes result must be copied: {dart}"
        );
        // Borrowed: the callback must not release the producer's buffers.
        let cb = &dart[dart
            .find("Future<List<String>> fetchNames()")
            .expect("wrapper")..];
        let cb = &cb[..cb.find("\n}").expect("end")];
        assert!(
            !cb.contains("_weaveffiFree"),
            "async callback must never free borrowed result buffers: {cb}"
        );
    }

    /// A buffered async *input* is staged like a sync input and released only
    /// when the future completes (or the launch throws).
    #[test]
    fn async_buffered_input_staged_until_completion() {
        let api = make_api(vec![Module {
            name: "jobs".into(),
            functions: vec![Function {
                r#async: true,
                ..func(
                    "submit",
                    vec![param("tags", TypeRef::List(Box::new(TypeRef::StringUtf8)))],
                    Some(TypeRef::I64),
                )
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
            dart.contains("final tagsPtr = _stageBytes(tagsBuf);"),
            "missing staged async input: {dart}"
        );
        assert!(
            dart.contains("_weaveffiJobsSubmitAsync(tagsPtr, tagsBuf.length, callable.nativeFunction, nullptr);"),
            "launcher must pass the (ptr, len) pair: {dart}"
        );
        assert!(
            dart.contains("return completer.future.whenComplete(() {")
                && dart.contains("calloc.free(tagsPtr);"),
            "staged input must be freed on completion: {dart}"
        );
    }

    /// Buffered callback/listener arguments are borrowed (ptr, len) pairs
    /// valid only during the dispatch: the trampoline decodes them before
    /// invoking the user's closure and never frees them.
    #[test]
    fn listener_buffered_argument_decoded_in_borrow_window() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Event".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
            }],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "on_event".into(),
                params: vec![param("event", TypeRef::Record("Event".into()))],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "events".into(),
                event_callback: "on_event".into(),
                doc: None,
            }],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        // The native callback typedef carries the (ptr, len) pair + context.
        assert!(
            dart.contains(
                "typedef _NativeCb_weaveffi_events_on_event_fn = Void Function(Pointer<Uint8>, Size, Pointer<Void>);"
            ),
            "missing buffered callback typedef: {dart}"
        );
        // The trampoline decodes inside the borrow window, then dispatches.
        assert!(
            dart.contains("(Pointer<Uint8> eventPtr, int eventLen, Pointer<Void> context) {"),
            "missing trampoline slots: {dart}"
        );
        assert!(
            dart.contains("final eventData = _copyNativeBytes(eventPtr, eventLen);")
                && dart.contains("final eventValue = _unpackEvent(eventReader);")
                && dart.contains("callback(eventValue);"),
            "trampoline must decode before invoking the user callback: {dart}"
        );
        // Borrowed: never freed by the consumer.
        assert!(
            !dart.contains("_weaveffiFreeBytes(eventPtr"),
            "borrowed callback argument must not be freed: {dart}"
        );
        // Register/unregister plumbing is unchanged.
        assert!(
            dart.contains("int registerEvents(void Function(Event event) callback) {")
                && dart.contains("void unregisterEvents(int id) {"),
            "missing listener wrappers: {dart}"
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
