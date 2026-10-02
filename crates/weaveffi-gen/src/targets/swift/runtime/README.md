# {{MODULE}} (Swift)

A SwiftPM package (product and module `{{MODULE}}`) over the native library
`{{LIBRARY}}`, whose C declarations are the module `{{C_MODULE}}`.

## Linking the bundled library

The prebuilt libraries are bundled under `lib/<platform>/`. Without further
setup the package links `{{LIBRARY}}` from the linker search path:

```bash
swift build -Xlinker -L"$PWD/lib/darwin-arm64"
```

## iOS and the XCFramework

When `{{C_MODULE}}.xcframework` sits next to `Package.swift`, the manifest uses
it as a binary target instead, so apps (including iOS apps) link the library
with no flags. `weaveffi package --target swift` assembles it on macOS
whenever the requested platforms include an iOS slice, for example
`--platforms ios-arm64,ios-sim-arm64,darwin-arm64`.

## Bundled platforms

{{PLATFORMS}}
