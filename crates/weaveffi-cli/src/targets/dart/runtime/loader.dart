// ── Library loading ──
// The native library opens on first use and is verified before any other
// lookup: it must implement the C ABI revision these bindings were generated
// against and carry every declaration they were generated with, unchanged.

/// The native library every binding in this package calls. A failed load
/// throws from whichever call came first and is tried again on the next one.
final DynamicLibrary _lib = _verifyLibrary(_openLibrary());

/// The native library couldn't be loaded, or it doesn't match these
/// bindings: a different C ABI revision, or a declaration that's missing or
/// changed. Regenerate the bindings from the library you ship.
///
/// It's thrown by the first call into the library, and the next call tries
/// to load it again, so an app can catch it, report it, and carry on.
final class NativeLibraryException implements Exception {
  /// Creates a load failure described by [message].
  NativeLibraryException(this.message);

  /// What failed, naming the library or declaration.
  final String message;

  @override
  String toString() => 'NativeLibraryException: $message';
}

/// The platform file name of the library on the system search path.
String get _libraryFile => Platform.isMacOS
    ? '{{MACOS_FILE}}'
    : Platform.isWindows
        ? '{{WINDOWS_FILE}}'
        : '{{LINUX_FILE}}';

DynamicLibrary _openLibrary() {
  // An explicit path wins, so a caller can point at any build artifact.
  final override = Platform.environment['{{LIBRARY_ENV}}'];
  if (override != null && override.isNotEmpty) {
    return _open(override);
  }
  // iOS links native code into the app binary.
  if (Platform.isIOS) return DynamicLibrary.process();
{{BUNDLED_LOOKUP}}  try {
    return DynamicLibrary.open(_libraryFile);
  } on ArgumentError catch (e) {
    // A macOS app may link the library into its executable instead.
    if (Platform.isMacOS) return DynamicLibrary.process();
    throw NativeLibraryException("can't open '$_libraryFile' ($e); set "
        '{{LIBRARY_ENV}} to its path');
  }
}

DynamicLibrary _open(String path) {
  try {
    return DynamicLibrary.open(path);
  } on ArgumentError catch (e) {
    throw NativeLibraryException("can't open '$path': $e");
  }
}

/// The C ABI revision these bindings were generated against.
const int _abiVersion = {{ABI_VERSION}};

/// One `{{PREFIX}}_contract_entry` of a module's contract table.
final class _ContractEntry extends Struct {
  @Uint64()
  external int id;
  @Uint64()
  external int hash;
}

DynamicLibrary _verifyLibrary(DynamicLibrary lib) {
  final int Function() abiVersion;
  try {
    abiVersion = lib.lookupFunction<Uint32 Function(), int Function()>(
        '{{PREFIX}}_abi_version');
  } on ArgumentError {
    throw NativeLibraryException(
        "the native library '{{LIBRARY}}' isn't loaded; set {{LIBRARY_ENV}} "
        'to its path');
  }
  final found = abiVersion();
  if (found != _abiVersion) {
    throw NativeLibraryException('{{NAME}}: these bindings expect C ABI '
        'revision $_abiVersion, but the loaded library implements revision '
        '$found');
  }
  for (final (symbol, expected) in _contracts) {
    _checkContract(lib, symbol, expected);
  }
  return lib;
}

/// Checks that every `(id, hash, path)` entry in [expected] is in the table
/// [symbol] returns with an equal hash. Entries only the library has are
/// fine: they're declarations (or error codes, or callback methods) added
/// after these bindings were generated.
void _checkContract(
    DynamicLibrary lib, String symbol, List<(int, int, String)> expected) {
  final Pointer<_ContractEntry> Function(Pointer<Size>) table;
  try {
    table = lib.lookupFunction<Pointer<_ContractEntry> Function(Pointer<Size>),
        Pointer<_ContractEntry> Function(Pointer<Size>)>(symbol);
  } on ArgumentError {
    throw NativeLibraryException(
        "{{NAME}}: the loaded library doesn't export '$symbol'");
  }
  final hashes = <int, int>{};
  final len = calloc<Size>();
  try {
    final entries = table(len);
    for (var i = 0; i < len.value; i++) {
      hashes[entries[i].id] = entries[i].hash;
    }
  } finally {
    calloc.free(len);
  }
  for (final (id, hash, path) in expected) {
    final found = hashes[id];
    if (found == null) {
      throw NativeLibraryException(
          '{{NAME}}: $path is missing from the library');
    }
    if (found != hash) {
      throw NativeLibraryException(
          '{{NAME}}: $path changed since these bindings were generated');
    }
  }
}
