# Naming

Every name WeaveFFI emits derives from two things: the library's
**identity** (what the library is called) and the **IDL names** (what the
API calls things). This page states both sets of rules.

## Identity

The CLI resolves one identity per library from `weaveffi.toml` and, for a
Rust producer, `Cargo.toml` (the full rules are in
[Project Configuration](../guides/config.md#package)):

| Field | Meaning | Rust producer | IDL |
|-------|---------|---------------|-----|
| `name` | package name as published | `[package] name`, else the crate's package name | `[package] name`, else the input file stem |
| `prefix` | C symbol prefix | the crate's library name | `[package] c_prefix`, else snake(`name`) |
| `library` | native library base name | same as `prefix` | `[package] library`, else snake(`name`) |
| `version`, `description`, `license`, `authors`, `homepage`, `repository` | metadata | `[package]`, falling back to `Cargo.toml` | `[package]` |

Two conversions turn an arbitrary name into identifiers:

- **snake**: lowercase ASCII letters and digits, with every run of other
  characters collapsed to one `_` and trailing `_` trimmed; a leading digit
  gets a `lib_` prefix. `my-kv.store` becomes `my_kv_store`, `3d-engine`
  becomes `lib_3d_engine`.
- **PascalCase**: each run of letters and digits starts a word whose first
  letter is uppercased, and existing capitals are kept; a leading digit gets
  an `N` prefix. `my-kv.store` becomes `MyKvStore`.

`{PREFIX}` is the prefix uppercased.

## Per-target defaults

| Target | Default names (each overridable in `[generators.<target>]`) |
|--------|---------------------------------------------------------------|
| C | header `{library}.h`; include guard `{PREFIX}_H`; export macro `{PREFIX}_API` |
| C++ | header `{library}.hpp`, which includes `{library}.h` (shipped alongside); namespace `{prefix}` |
| Swift | SwiftPM package, product, and module `PascalCase(name)`; C module `C{PascalCase(name)}` |
| Kotlin | package `{prefix}`; one Kotlin `object` per IDL module; JNI library `{library}_jni` |
| Node.js | npm package `{name}`; addon `{library}_node.node`; one namespace export per module |
| WebAssembly | npm package `{name}`; ES module; one namespace export per module |
| Python | distribution `{name}`; import package `{prefix}` |
| .NET | namespace and assembly `PascalCase(name)` |
| Dart | package `{prefix}` |
| Go | module path `{name}` unless `module_path` is set; package `{prefix}` |
| Ruby | gem `{name}`; module `PascalCase(name)`; require path `{prefix}` |

For the `kvstore` sample (crate `kvstore`) that's `kvstore.h`, Swift module
`Kvstore`, `import kvstore` in Python, and `require "kvstore"` in Ruby.

**Library loading.** Every target loads the native library by its
`library` name using the platform's file naming (`lib{library}.dylib`,
`lib{library}.so`, `{library}.dll`). Every loader first honors the
environment variable `{PREFIX}_LIBRARY` (for example `KVSTORE_LIBRARY`), an
explicit path that wins over bundled and system copies.

Nothing a generator emits is named after WeaveFFI; the only mentions are the
generated-file header comment and runtime-version comments. Two libraries
built with WeaveFFI therefore never collide in symbols, packages, types, or
environment variables.

## C identifiers

C identifiers are the prefix, the underscore-joined module path, and the IDL
name, verbatim: `kvstore_kv_Store_open`, `kvstore_kv_KvError_KeyNotFound`.
IDL names are never re-cased in C, except that an iterator type's function
part is PascalCase (`kvstore_kv_Store_ListKeysIterator`). The full table is
in the [C ABI contract](abi.md#symbol-names); validation rejects any API in
which two declarations produce the same identifier.

## Identifiers in generated code

IDL names are expected in `snake_case` for modules, functions, parameters,
and fields, and in `PascalCase` for types, variants, and error codes. The
validator only requires identifiers, so other styles are re-cased on a
best-effort basis.

**Functions, methods, parameters, and fields** follow each language's
convention:

| Target | Style | `get_stats`, `created_at` |
|--------|-------|---------------------------|
| C, C++, Python, Ruby | `snake_case` | `get_stats`, `created_at` |
| Swift, Kotlin, Node.js, WebAssembly, Dart | `camelCase` | `getStats`, `createdAt` |
| Go, .NET | `PascalCase` | `GetStats`, `CreatedAt` |

**Types** (records, enums, interfaces, callback interfaces, error domains)
keep their IDL names in every target, except where a language idiom adds a
prefix or suffix (.NET prefixes interfaces it generates for callback
interfaces with `I`, for example). **C-style enum variants** follow the
target's enum idiom: `Volatile` in most targets, `.volatile` in Swift,
`EntryKindVolatile` in Go, `EntryKind::VOLATILE` in Ruby.

**Constructors.** A constructor named `new` becomes the language's
constructor (`init`, `__init__`, `initialize`, a C++ or C# constructor);
other constructors become static factories (`Store.open`). Go has no
constructors, so they become package functions (`NewEventBus`, `OpenStore`).

**Modules.** How module members are grouped depends on the target's notion
of a namespace. Kotlin emits one `object` per IDL module, and Node.js and
WebAssembly export one namespace object per module (nested modules nest),
so two modules can both declare `get`. Targets that place members in one
flat namespace drop the module name from free functions by default and
offer `strip_module_prefix = false` to keep it (`kv_stats_get_stats`); see
[Project Configuration](../guides/config.md#global). Each language page
documents its layout.

**Errors.** Each target has a root error type named after the package and
one type (or case) per error domain and code, named from the IDL with at
most one idiomatic suffix (`Error` or `Exception`). Write code names without
a suffix (`KeyNotFound`, not `KeyNotFoundError`) and let the generator add
it. Code names are unique across the API because several targets flatten
them into one namespace.

**Reserved words.** A name that's a keyword in the target language gains a
trailing `_` (`type` becomes `type_`), a rule that's stable under repetition.
A user type whose name would shadow a standard type in the target (a Kotlin
type named `Result`, say) is referenced in a way that avoids the clash; the
language pages describe how.

## Target names

A target is named after what its consumer identifies with: the language when
the output is idiomatic to one language (`swift`, `python`, `kotlin`), and
the runtime when several languages consume it equally or two targets share a
language (`dotnet` serves C#, F#, and Visual Basic; `node` and `wasm` both
emit JavaScript). A deployment platform is never a target name: `kotlin`
covers Android and the desktop JVM, selected by a setting. The same token
names the `--target` value, the `[generators.<target>]` table, the output
directory, and the page under [Generators](../generators/README.md).
