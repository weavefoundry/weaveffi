//! The private Dart runtime every generated library ships: the dynamic
//! library loaders, the error struct and generic exception plumbing, the
//! value-buffer writer/reader pair, the callback-interface handle table, and
//! the `lookupFunction` binding helper.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::cabi::ABI_VERSION;
use weaveffi_core::platform::{Os, Platform};

/// The reserved `weaveffi_error.code` a callback-interface trampoline reports
/// when the Dart implementation raised (`FOREIGN_ERROR_CODE` in the ABI).
pub(crate) const FOREIGN_ERROR_CODE: i32 = -4;

/// Emit `_checkAbiVersion`, which the `_lib` initializer routes the freshly
/// opened library through. It runs before any other lookup, so a producer
/// built for a different ABI revision (or one predating versioning) fails
/// with a clear `StateError` the first time the library is touched.
pub(crate) fn render_abi_version_check(out: &mut String) {
    out.push_str("// The ABI revision these bindings were generated against.\n");
    out.push_str(&format!("const int _abiVersion = {ABI_VERSION};\n\n"));
    out.push_str("DynamicLibrary _checkAbiVersion(DynamicLibrary lib) {\n");
    out.push_str("  final int Function() abiVersion;\n");
    out.push_str("  try {\n");
    out.push_str(
        "    abiVersion = lib.lookupFunction<Uint32 Function(), int Function()>('weaveffi_abi_version');\n",
    );
    out.push_str("  } on ArgumentError {\n");
    out.push_str("    throw StateError(\n");
    out.push_str("      'the loaded WeaveFFI library predates ABI versioning '\n");
    out.push_str("      '(these bindings expect ABI revision $_abiVersion)',\n");
    out.push_str("    );\n");
    out.push_str("  }\n");
    out.push_str("  final found = abiVersion();\n");
    out.push_str("  if (found != _abiVersion) {\n");
    out.push_str("    throw StateError(\n");
    out.push_str("      'WeaveFFI ABI mismatch: these bindings expect revision $_abiVersion '\n");
    out.push_str("      'but the loaded library reports revision $found',\n");
    out.push_str("    );\n");
    out.push_str("  }\n");
    out.push_str("  return lib;\n");
    out.push_str("}\n\n");
}

/// Reproduce the exact `_openLibrary` block the module renderer emits in
/// `generate` mode for `lib_base`, so the packager can swap it.
pub(crate) fn dart_loader_original(lib_base: &str) -> String {
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

/// Whether the Dart package bundles a prebuilt library for `platform`: the
/// `dart:ffi` loader resolves `native/<platform-id>/` for the desktop matrix
/// only (Android libraries ship through Flutter's `jniLibs`, and a `.wasm`
/// module can't be opened with `DynamicLibrary`).
pub(crate) fn bundles_platform(platform: Platform) -> bool {
    platform.is_desktop()
}

/// The bundled candidate paths the packaged loader tries for one OS family,
/// in [`Platform::DESKTOP`] order, followed by the bare system library name.
fn loader_candidates(os: Os, lib: &str) -> String {
    let mut candidates: Vec<String> = Platform::DESKTOP
        .iter()
        .filter(|p| p.os() == os)
        .map(|p| format!("'native/{}/{}'", p.id(), p.lib_filename(lib)))
        .collect();
    let bare = Platform::DESKTOP
        .iter()
        .find(|p| p.os() == os)
        .map_or_else(|| format!("lib{lib}.so"), |p| p.lib_filename(lib));
    candidates.push(format!("'{bare}'"));
    candidates.join(", ")
}

/// The packaged `_openLibrary` for `lib`: try the bundled `native/<platform>/`
/// libraries (relative to the working directory) before the bare system name.
/// `WEAVEFFI_LIBRARY` still overrides.
pub(crate) fn dart_loader_packaged(lib: &str) -> String {
    let mut out = String::new();
    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str("  final override = Platform.environment['WEAVEFFI_LIBRARY'];\n");
    out.push_str(
        "  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);\n",
    );
    out.push_str("  final candidates = <String>[];\n");
    out.push_str("  if (Platform.isMacOS) {\n");
    out.push_str(&format!(
        "    candidates.addAll([{}]);\n",
        loader_candidates(Os::MacOs, lib)
    ));
    out.push_str("  } else if (Platform.isWindows) {\n");
    out.push_str(&format!(
        "    candidates.addAll([{}]);\n",
        loader_candidates(Os::Windows, lib)
    ));
    out.push_str("  } else {\n");
    out.push_str(&format!(
        "    candidates.addAll([{}]);\n",
        loader_candidates(Os::Linux, lib)
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

/// Emit the private Dart value-buffer runtime: the little-endian, packed
/// writer and reader plus the staging/copy helpers wrappers use to move
/// encodings across the boundary.
pub(crate) fn render_buffer_runtime(out: &mut String) {
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

/// Emit the shared callback-interface runtime: the handle table that keeps
/// Dart implementations alive while the producer holds them, the key
/// allocator whose value crosses as `ctx`, the `NativeCallable` anchor list
/// that pins every vtable trampoline for the process lifetime, and the
/// `_foreignError` reporter trampolines route a thrown exception through.
pub(crate) fn render_callback_runtime(out: &mut String) {
    out.push_str(&format!(
        r#"
// ── WeaveFFI callback-interface runtime ──
// A Dart implementation passed to the producer is stored here under an
// integer key; the key (widened to a pointer) is what crosses as `ctx`, so the
// producer never holds a Dart object and the GC never sees a raw pointer. The
// entry lives until the producer calls the vtable's `free(ctx)`.
final Map<int, Object> _callbackTable = {{}};
int _nextCallbackKey = 1;

Pointer<Void> _registerCallback(Object impl) {{
  final key = _nextCallbackKey++;
  _callbackTable[key] = impl;
  return Pointer<Void>.fromAddress(key);
}}

Object _callbackFor(Pointer<Void> ctx) {{
  final impl = _callbackTable[ctx.address];
  if (impl == null) {{
    throw StateError('callback interface context ${{ctx.address}} is not registered');
  }}
  return impl;
}}

// Vtable trampolines are process-wide: their NativeCallables are never closed,
// so they are anchored here to keep the native thunks alive. `keepIsolateAlive`
// is cleared on each so an idle isolate can still exit.
final List<NativeCallable<Function>> _callbackCallables = [];

Pointer<NativeFunction<T>> _pinCallable<T extends Function>(
    NativeCallable<T> callable) {{
  callable.keepIsolateAlive = false;
  _callbackCallables.add(callable);
  return callable.nativeFunction;
}}

// Reports a Dart exception thrown by a callback-interface implementation
// through `weaveffi_error_set` with the reserved foreign error code ({code}).
// The producer copies the message, so the temporary is freed right away. This
// must never throw: it runs in the catch path of a trampoline whose C frame
// an exception must not unwind through.
void _foreignError(Pointer<_WeaveFFIError> outErr, Object error) {{
  if (outErr == nullptr) return;
  Pointer<Utf8> message = nullptr;
  try {{
    message = error.toString().toNativeUtf8();
    _weaveffiErrorSet(outErr, WeaveFFIException.foreignCode, message);
  }} catch (_) {{
    _weaveffiErrorSet(outErr, WeaveFFIException.foreignCode, nullptr);
  }} finally {{
    if (message != nullptr) calloc.free(message);
  }}
}}
"#,
        code = FOREIGN_ERROR_CODE
    ));
}

/// Emit the dart:ffi typedef pair and `lookupFunction` binding for one C
/// symbol.
pub(crate) fn emit_typedef_and_lookup(
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

/// Emit the shared error plumbing: the `_WeaveFFIError` struct mirroring the
/// C `weaveffi_error`, the runtime release lookups, the generic branded
/// exception, and the `_checkError` trap helper non-throwing wrappers route
/// their out-err slots through.
pub(crate) fn render_error_plumbing(out: &mut String) {
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

    // Callback-interface trampolines report a failed Dart implementation
    // through `weaveffi_error_set`, which copies the borrowed message with the
    // producer's allocator (the producer frees `message` itself, so the
    // consumer must never write the struct directly).
    emit_typedef_and_lookup(
        out,
        "weaveffi_error_set",
        "Pointer<_WeaveFFIError>, Int32, Pointer<Utf8>",
        "Pointer<_WeaveFFIError>, int, Pointer<Utf8>",
        "Void",
        "void",
    );
    emit_typedef_and_lookup(
        out,
        "weaveffi_error_clear",
        "Pointer<_WeaveFFIError>",
        "Pointer<_WeaveFFIError>",
        "Void",
        "void",
    );

    // Async completion callbacks receive a heap-boxed error the consumer
    // owns; `weaveffi_error_free` releases the message, the payload, and the
    // box itself once the deferred listener has copied what it needs.
    emit_typedef_and_lookup(
        out,
        "weaveffi_error_free",
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
        out,
        "weaveffi_free_string",
        "Pointer<Utf8>",
        "Pointer<Utf8>",
        "Void",
        "void",
    );
    emit_typedef_and_lookup(
        out,
        "weaveffi_free_bytes",
        "Pointer<Uint8>, Size",
        "Pointer<Uint8>, int",
        "Void",
        "void",
    );

    out.push_str("\n/// Generic WeaveFFI failure: a producer panic ([panicCode]), a marshalling\n");
    out.push_str("/// error ([marshalCode]), a callback-interface implementation that threw\n");
    out.push_str("/// ([foreignCode], carrying the Dart exception's text), or an unknown code.\n");
    out.push_str("class WeaveFFIException implements Exception {\n");
    out.push_str("  /// The producer reported an untyped error.\n");
    out.push_str("  static const int genericCode = -1;\n");
    out.push_str("  /// The producer panicked; [message] carries the panic text.\n");
    out.push_str("  static const int panicCode = -2;\n");
    out.push_str("  /// An argument could not be lifted by the producer.\n");
    out.push_str("  static const int marshalCode = -3;\n");
    out.push_str("  /// A Dart callback-interface implementation threw; [message] carries the\n");
    out.push_str("  /// exception's text.\n");
    out.push_str(&format!(
        "  static const int foreignCode = {FOREIGN_ERROR_CODE};\n"
    ));
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
}
