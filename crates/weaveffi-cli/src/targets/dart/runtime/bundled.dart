
/// The library bundled for the running platform under `native/<platform>/`,
/// relative to the package root, or null when none is bundled for it.
String? get _bundledLibrary => switch (Abi.current()) {
{{BUNDLED_CASES}}      _ => null,
    };

/// The packaged library's root directory (the parent of its `lib/`), or null
/// when the package can't be resolved (a compiled executable, say).
String? _packageRoot() {
  try {
    final uri = Isolate.resolvePackageUriSync(
        Uri.parse('package:{{PACKAGE}}/{{PACKAGE}}.dart'));
    if (uri == null || !uri.isScheme('file')) return null;
    return File.fromUri(uri).parent.parent.path;
  } on UnsupportedError {
    return null;
  }
}

/// Opens the bundled library from the package, then from the working
/// directory (where an app may have copied `native/`), or returns null.
DynamicLibrary? _openBundled() {
  final relative = _bundledLibrary;
  if (relative == null) return null;
  for (final root in [_packageRoot(), Directory.current.path]) {
    if (root == null) continue;
    final path = '$root${Platform.pathSeparator}$relative';
    if (File(path).existsSync()) return _open(path);
  }
  return null;
}
