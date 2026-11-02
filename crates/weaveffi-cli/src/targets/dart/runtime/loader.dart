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
  for (final (symbol, hash, path) in _contracts) {
    if (!lib.providesSymbol(symbol)) {
      throw NativeLibraryException(
          '{{NAME}}: $path is missing from the library');
    }
    final contract =
        lib.lookupFunction<Uint64 Function(), int Function()>(symbol);
    if (contract() != hash) {
      throw NativeLibraryException(
          '{{NAME}}: $path changed since these bindings were generated');
    }
  }
  return lib;
}
