# Generator Configuration

WeaveFFI reads an optional TOML configuration file that lets you customise
names, packages, and prefixes used by each code generator. When no configuration
file is provided, every option falls back to a sensible default.

## Passing the configuration file

Use the `--config` flag on the `generate` command:

```bash
weaveffi generate api.yml -o generated --config weaveffi.toml
```

When `--config` is omitted, all options use their default values.

## File format

The configuration file is plain TOML. All keys are top-level — there are no
nested tables. Every key is optional; omit a key to keep its default.

### Minimal example

An empty file (or no file at all) is valid — defaults apply to everything:

```toml
# weaveffi.toml — all defaults
```

### Full example

```toml
# weaveffi.toml

# Swift module name used in Package.swift and the Sources/ directory.
swift_module_name = "MyApp"

# Java/Kotlin package name for the Android JNI wrapper.
android_package = "com.example.myapp"

# npm package name emitted in the Node.js loader.
node_package_name = "@myorg/myapp"

# WASM module name used in the JavaScript loader.
wasm_module_name = "myapp_wasm"

# Prefix for C ABI symbol names (e.g. myapp_math_add instead of weaveffi_math_add).
c_prefix = "myapp"

# When true, strip the module name from generated identifiers where applicable.
strip_module_prefix = true
```

## Configuration options

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `swift_module_name` | string | `"WeaveFFI"` | Name of the Swift module in `Package.swift` and the `Sources/` directory. |
| `android_package` | string | `"com.weaveffi"` | Java/Kotlin package declaration in the generated JNI wrapper. |
| `node_package_name` | string | `"weaveffi"` | Package name in the generated Node.js N-API loader. |
| `wasm_module_name` | string | `"weaveffi_wasm"` | Module name in the generated WASM JavaScript loader. |
| `c_prefix` | string | `"weaveffi"` | Prefix prepended to every C ABI symbol (`{prefix}_{module}_{function}`). |
| `strip_module_prefix` | bool | `false` | Strip the module name from generated identifiers where applicable. |

### `swift_module_name`

Controls the Swift package and module name. The generated `Package.swift`
references this name, and the source directory is created as
`Sources/{swift_module_name}/`.

```toml
swift_module_name = "CoolLib"
```

Produces `Sources/CoolLib/CoolLib.swift` and a matching `Package.swift`:

```swift
// Package.swift
let package = Package(
    name: "CoolLib",
    ...
    targets: [
        .target(name: "CoolLib", path: "Sources/CoolLib"),
    ]
)
```

### `android_package`

Sets the Java/Kotlin package for the generated JNI bridge. The package
determines the directory structure and the `package` declaration at the top of
the generated `.kt` file.

```toml
android_package = "com.example.myapp"
```

Produces:

```kotlin
package com.example.myapp
```

### `node_package_name`

Sets the package name used in the generated Node.js loader. This is the name
consumers use in `require()` or `import` statements.

```toml
node_package_name = "@myorg/cool-lib"
```

### `wasm_module_name`

Sets the module name used in the generated WASM JavaScript loader and
TypeScript declarations.

```toml
wasm_module_name = "coolapp_wasm"
```

### `c_prefix`

Replaces the default `weaveffi` prefix on all C ABI symbol names. This affects
every generated header and every language binding that references C symbols.

```toml
c_prefix = "myapp"
```

With an API module named `math` containing a function `add`, the exported C
symbol becomes `myapp_math_add` instead of the default `weaveffi_math_add`.

### `strip_module_prefix`

When set to `true`, generated identifiers omit the module name where the
target language supports namespacing natively.

```toml
strip_module_prefix = true
```

## Common recipes

### iOS/macOS project

```toml
swift_module_name = "MyAppFFI"
c_prefix = "myapp"
```

```bash
weaveffi generate api.yml -o generated -t swift,c --config weaveffi.toml
```

### Android project

```toml
android_package = "com.example.myapp.ffi"
c_prefix = "myapp"
```

```bash
weaveffi generate api.yml -o generated -t android --config weaveffi.toml
```

### Node.js package

```toml
node_package_name = "@myorg/myapp-native"
```

```bash
weaveffi generate api.yml -o generated -t node --config weaveffi.toml
```

### All targets with custom prefix

```toml
swift_module_name = "MyAppFFI"
android_package = "com.example.myapp"
node_package_name = "@example/myapp"
wasm_module_name = "myapp_wasm"
c_prefix = "myapp"
```

```bash
weaveffi generate api.yml -o generated --config weaveffi.toml
```
