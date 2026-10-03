// ── Library loading ──
// The native library opens on first use and is verified before any other
// lookup: its C ABI revision and every module's contract checksum must match
// the values these bindings were generated against.

/// The native library every binding in this file calls.
final DynamicLibrary _lib = _verifyLibrary(_openLibrary());

DynamicLibrary _openLibrary() {
  // An explicit path wins, so a caller can point at any build artifact.
  final override = Platform.environment['{{LIBRARY_ENV}}'];
  if (override != null && override.isNotEmpty) {
    return DynamicLibrary.open(override);
  }
  // iOS links native code into the app binary.
  if (Platform.isIOS) return DynamicLibrary.process();
  final candidates = Platform.isMacOS
      ? const <String>[{{MACOS_CANDIDATES}}]
      : Platform.isWindows
          ? const <String>[{{WINDOWS_CANDIDATES}}]
          : const <String>[{{LINUX_CANDIDATES}}];
  ArgumentError? failure;
  for (final candidate in candidates) {
    try {
      return DynamicLibrary.open(candidate);
    } on ArgumentError catch (e) {
      failure ??= e;
    }
  }
  // A macOS app may link the library into its executable instead.
  if (Platform.isMacOS) return DynamicLibrary.process();
  throw failure!;
}

/// The C ABI revision these bindings were generated against.
const int _abiVersion = {{ABI_VERSION}};

/// Each top-level module's name, checksum symbol, and expected checksum.
const List<(String, String, int)> _contracts = <(String, String, int)>[
{{CONTRACTS}}];

DynamicLibrary _verifyLibrary(DynamicLibrary lib) {
  final int Function() abiVersion;
  try {
    abiVersion = lib.lookupFunction<Uint32 Function(), int Function()>(
        '{{PREFIX}}_abi_version');
  } on ArgumentError {
    throw StateError(
        "the native library '{{LIBRARY}}' isn't loaded; set {{LIBRARY_ENV}} to "
        'its path');
  }
  final found = abiVersion();
  if (found != _abiVersion) {
    throw StateError('{{NAME}}: these bindings expect C ABI revision '
        '$_abiVersion, but the loaded library reports revision $found');
  }
  for (final (module, symbol, expected) in _contracts) {
    final int actual;
    try {
      actual = lib.lookupFunction<Uint64 Function(), int Function()>(symbol)();
    } on ArgumentError {
      throw StateError(
          "{{NAME}}: the loaded library doesn't export module '$module'");
    }
    if (actual != expected) {
      throw StateError("{{NAME}}: module '$module' doesn't match the loaded "
          'library (contract checksum ${_hex(actual)}, expected '
          '${_hex(expected)}); regenerate the bindings');
    }
  }
  return lib;
}

String _hex(int v) => '0x${v.toUnsigned(64).toRadixString(16).padLeft(16, '0')}';
