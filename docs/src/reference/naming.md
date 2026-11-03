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
  characters collapsed to one `_` and leading and trailing `_` dropped; a
  leading digit gets a `lib_` prefix. `my-kv.store` becomes `my_kv_store`,
  `3d-engine` becomes `lib_3d_engine`.
- **PascalCase**: each run of letters and digits starts a word whose first
  letter is uppercased, and existing capitals are kept; a leading digit gets
  an `N` prefix. `my-kv.store` becomes `MyKvStore`.

`{P}` is the prefix uppercased. A `c_prefix` you set is itself passed
through snake. Setting `c_prefix` or `library` for a Rust producer is an
error, because the attributes derive the prefix from the crate. A library
read on its own with `--library` (no crate or IDL) takes its prefix from its
embedded metadata, its `library` from its file name, and its `name` from
`[package] name`, else the prefix.

## Package names

Each target names its package, module, or namespace after the identity, and
a `[generators.<target>]` table can override it with `name` (see
[Project Configuration](../guides/config.md#generatorstarget)). By default:

| Target | Default names |
|--------|---------------|
| C | header `{library}.h` (and `{library}_buffer.h`); include guard `{P}_H`; export macro `{P}_API` |
| C++ | header `{library}.hpp`, which includes `{library}.h`; one namespace `{library}` |
| Swift | SwiftPM package, product, and module `PascalCase(name)` |
| Kotlin | package `{prefix}`; free functions on an `object` named `PascalCase(name)` |
| Node.js, WebAssembly | npm package `{name}` |
| Python | distribution `{name}`; import package `{prefix}` |
| .NET | namespace, assembly, and NuGet id `PascalCase(name)`; free functions on the static class `{Namespace}Library` |
| Dart | package `{prefix}` |
| Go | module path `{name}`; package `{prefix}` without its underscores |
| Ruby | gem `{name}`; module `PascalCase(name)` |

Each language page is authoritative for its names and their overrides. For
the `kvstore` sample (crate `kvstore`) that's `kvstore.h`, the Swift module
`Kvstore`, and `import kvstore` in Python.

**Library loading.** Every target finds the native library by its `library`
name using the platform's file naming (`lib{library}.dylib`,
`lib{library}.so`, `{library}.dll`). The targets that load the library at
run time also accept an explicit path in the environment variable
`{P}_LIBRARY` (for example `KVSTORE_LIBRARY`), which wins over bundled and
system copies; the targets that link it when they're built (C, C++, Swift,
and Go) find it through the platform linker.

Nothing a generator emits is named after WeaveFFI; the only mentions are
comments (the generated-file header and notes on regenerating or
packaging) and the file name of the Swift runtime source,
`WeaveFFIRuntime.swift`, which declares nothing named after WeaveFFI. Two libraries built with WeaveFFI therefore never collide in
symbols, packages, types, or environment variables.

## One namespace

A library is one namespace. Record, enum, error domain, interface, callback
interface, and free-function names are unique across the whole library
(`DuplicateName`), and error-code names are unique across every domain
(`DuplicateErrorCodeName`). That holds however the definition is split: an
IDL across files with `imports:`, a Rust crate across modules and files. A
type reference is therefore always the bare name. Member names (parameters,
fields, variants, interface members, and callback methods) are scoped to
their owner.

## C identifiers

C identifiers are the prefix and the IDL name, verbatim: `kvstore_open_store`
for a function, `kvstore_Store_open` for a member, `kvstore_KvError_KeyNotFound`
for an error code. IDL names are never re-cased in C, except that an
iterator type's function part is PascalCase (`kvstore_Store_KeysIterator`
for `Store.keys`). The full table is in the
[C ABI contract](abi.md#signatures); validation rejects any API in which two
declarations produce the same identifier (`SymbolCollision`).

## Identifiers in generated code

IDL names are expected in `snake_case` for functions, parameters, and
fields, and in `PascalCase` for types, variants, and error codes. The
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
prefix or suffix (an `I` on a .NET interface, for example). **C-style enum
variants** follow the target's enum idiom (`.volatile` in Swift, say).

**Constructors.** A constructor named `new` becomes the language's
constructor (`init`, `__init__`, `initialize`, a C++ or C# constructor);
other constructors become static factories (`Store.open`). Each language
page describes its layout.

### Errors

Each target has a root error type and one type (or case) per error domain
and code, named from the IDL with at most one idiomatic suffix (`Error` or
`Exception`). The root is `Error` in C++, Python, Go, and Ruby (Python's is
`{PascalName}Error` when the API declares its own `Error`),
`{Module}RuntimeError` in Swift, `FfiException` in Kotlin, and
`NativeException` in .NET and Dart. Node.js and WebAssembly name it after
the package (`KvstoreError`), or add `Library` when a declaration already
has that name (`CodecLibraryError` beside the `codec` sample's `CodecError`
domain). Every target applies one shared
rule (`weaveffi_model::errors::type_name`): convert the name to PascalCase,
strip one trailing `Exceptions`, `Exception`, `Errors`, or `Error`, then add
the target's suffix. So `KvError` stays `KvError` (or becomes
`KvException`), `KitchenErrors` becomes `KitchenError`, `Failure` becomes
`FailureError`, and a code `KEY_NOT_FOUND` becomes `KeyNotFoundError`. Write
code names without a suffix (`KeyNotFound`, not `KeyNotFoundError`) and let
the generator add it.

### Reserved words

A name that's a keyword in the target language gains a trailing `_` (`type`
becomes `type_`), a rule that's stable under repetition. (Ruby allows
keywords as method names, so its members keep them: `entry.end`.) An interface member
whose target spelling matches a member the generated object wrapper declares
or inherits (its release method, say) gains a trailing `_` as well, so the
wrapper keeps working; each language page lists its reserved member names.
A free function whose name clashes with another name its target emits
gains a trailing `_` too: in Go, a free function spelled like a type, a
constructor, or a static (the `kvstore` sample's async `open_store` is
`OpenStore_`, because the constructor `Store.open` is `OpenStore`), and in
Node.js and WebAssembly a function named `init`, which the WebAssembly
package exports itself (`init_`).

In C, members are prefixed symbols, but a member named `clone` or `destroy`
lowers to the same symbol as the interface's own `_clone` or `_destroy`, so
validation rejects it (`SymbolCollision`).

## Target names

A target is named after what its consumer identifies with: the language when
the output is idiomatic to one language (`swift`, `python`, `kotlin`), and
the runtime when several languages consume it equally or two targets share a
language (`dotnet` serves C#, F#, and Visual Basic; `node` and `wasm` both
emit JavaScript). A deployment platform is never a target name: `kotlin`
covers Android and the desktop JVM. The same token names the `--target`
value, the `[generators.<target>]` table, the output directory, and the page
under [Generators](../generators/README.md).
