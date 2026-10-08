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

`{PREFIX}` is the prefix uppercased. A `c_prefix` you set is itself passed
through snake. Setting `c_prefix` or `library` for a Rust producer is an
error, because the macro derives the prefix from the crate. A library read
on its own with `--library` (no crate or IDL) takes its prefix from its
embedded metadata, its `library` from its file name, and its `name` from
`[package] name`, else the prefix.

## Per-target defaults

The names a `[generators.<target>]` key can override are listed in
[Project Configuration](../guides/config.md#generatorstarget); the rest
follow the identity.

| Target | Default names |
|--------|---------------|
| C | header `{library}.h`; include guard `{PREFIX}_H`; export macro `{PREFIX}_API` |
| C++ | header `{library}.hpp`, which includes `{library}.h` (shipped alongside); namespace `{prefix}` |
| Swift | SwiftPM package, product, and module `PascalCase(name)`; C module `C{PascalCase(name)}` |
| Kotlin | package `{prefix}`; one Kotlin `object` per IDL module; JNI library `{library}_jni` |
| Node.js | npm package `{name}`; addon `{library}_node.node`; one namespace export per module |
| WebAssembly | npm package `{name}`; ES module; one namespace export per module |
| Python | distribution `{name}`; import package `{prefix}` |
| .NET | namespace, assembly, and NuGet id `PascalCase(name)` |
| Dart | package `{prefix}` |
| Go | module path `{name}`; package `{prefix}` |
| Ruby | gem `{name}`; module `PascalCase(name)`; require path `{prefix}` |

For the `kvstore` sample (crate `kvstore`) that's `kvstore.h`, Swift module
`Kvstore`, `import kvstore` in Python, and `require "kvstore"` in Ruby.

**Library loading.** Every target finds the native library by its
`library` name using the platform's file naming (`lib{library}.dylib`,
`lib{library}.so`, `{library}.dll`). The environment variable
`{PREFIX}_LIBRARY` (for example `KVSTORE_LIBRARY`) names an explicit path
that wins over bundled and system copies where a target resolves the
library itself: at run time for Python, Ruby, .NET, Dart, Kotlin (the JVM
loads it before the JNI shim), and WebAssembly on Node.js (the `.wasm`
file); at install time for Node.js, whose fallback addon build links it; and
at CMake configure time for C++. C, Swift, and Go link the library at build
time and don't read it.

Nothing a generator emits is named after WeaveFFI; the only mentions are
comments (the generated-file header and notes on regenerating or
packaging). Two libraries built with WeaveFFI therefore never collide in
symbols, packages, types, or environment variables.

## Global IDL names

Type names (records, enums, interfaces, callback interfaces, and error
domains), free-function names, and error-code names are **global**: each is
unique across the whole API, and validation rejects a second declaration
anywhere in the module tree. Modules group declarations and namespace their
C symbols, but a name never needs its module to be unambiguous, so a type
reference in the IDL is always the bare name (`Store`, never `kv.Store`).
Member names (parameters, fields, variants, and interface members) are
scoped to their owner.

## C identifiers

C identifiers are the prefix, the underscore-joined path of the declaring
module, and the IDL name, verbatim: `kvstore_kv_Store_open`, `kvstore_kv_KvError_KeyNotFound`.
IDL names are never re-cased in C, except that an iterator type's function
part is PascalCase (`kvstore_kv_Store_KeysIterator` for `Store.keys`). The
full table is in the [C ABI contract](abi.md#symbol-names); validation
rejects any API in which two declarations produce the same identifier.

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
`volatile` in Dart, `EntryKindVolatile` in Go, `EntryKind::VOLATILE` in
Ruby.

**Constructors.** A constructor named `new` becomes the language's
constructor (`init`, `__init__`, `initialize`, a C++ or C# constructor);
other constructors become static factories (`Store.open`). Go has no
constructors, so they become package functions (`NewEventBus`, `OpenStore`).

**Modules.** How module members are grouped depends on the target's notion
of a namespace. C++ emits a nested namespace per IDL module, Swift a
caseless `enum` per module (nested modules nest), Kotlin an `object` per
module, .NET a static class per module named after its path (`Kv`,
`KvStats`), and Node.js and WebAssembly a namespace object per module
(nested modules nest). Targets that place members in one flat namespace
(Python, Ruby, Dart, Go) use the bare function name, which global names keep
collision-free; Go falls back to the module-prefixed spelling only for a
free function whose name would clash with another generated name, such as a
constructor's (kvstore's async `open_store` becomes `KvOpenStore` because
`Store.open` is `OpenStore`). Each language page documents its layout.

**Errors.** Each target has a root error type named after the package and
one type (or case) per error domain and code, named from the IDL with at
most one idiomatic suffix (`Error` or `Exception`). Write code names without
a suffix (`KeyNotFound`, not `KeyNotFoundError`) and let the generator add
it. Code names are global because several targets flatten them into one
namespace.

**Reserved words.** A name that's a keyword in the target language gains a
trailing `_` (`type` becomes `type_`), a rule that's stable under repetition.
A user type whose name would shadow a standard type in the target (a Kotlin
type named `Result`, say) is referenced in a way that avoids the clash; the
language pages describe how.

**Reserved member names.** An interface member (a constructor that becomes
a factory, a method, or a static) whose target spelling matches a member the
object wrapper declares or inherits gains a trailing `_` as well, so the
wrapper's own release, handle, and identity members keep working. The rule
applies after case conversion and keyword escaping, to these names:

| Target | Reserved member names |
|--------|-----------------------|
| C++ | `handle`, `clone_handle`, `raw_type` |
| Swift | `ptr`, `clonePtr`, `wvRead`, `wvWrite` |
| Kotlin | `close`, `handle`, `cloneHandle`, `fromHandle`, `fromHandleOrNull`, `invoke`, `equals`, `hashCode`, `toString`, `wait`, `notify`, `notifyAll` |
| Node.js, WebAssembly | methods: `close`, `constructor`; statics and factories: `name`, `length`, `prototype`, `caller` |
| Python | `close` |
| .NET | `Dispose`, `Handle`, `Adopt`, `CloneHandle`, `NativeHandle`, `Equals`, `GetHashCode`, `GetType`, `ToString`, `ReferenceEquals`, `MemberwiseClone`, `Finalize`, and the interface's own name |
| Dart | `dispose`, `hashCode`, `toString`, `runtimeType`, `noSuchMethod` |
| Go | methods: `Close` (factories and statics are package functions spelled with the type name, so they can't collide) |
| Ruby | methods: `close`, `handle`, `initialize`, `initialize_copy`, `class`, `clone`, `dup`, `freeze`, `hash`, `object_id`, `send`; statics and factories: `allocate`, `name` |

A method `close` on `Store` is therefore `close_` in Kotlin, Node.js,
WebAssembly, Python, and Ruby and `Close_` in Go, and a method `dispose` is
`dispose_` in Dart and `Dispose_` in .NET; every other target keeps the
plain spelling. C needs no rule, because members are prefixed symbols
(`kvstore_kv_Store_close`), but a member named `clone` or `destroy` lowers to
the same symbol as the interface's own `_clone` or `_destroy`, so validation
rejects it (`SymbolCollision`).

## Target names

A target is named after what its consumer identifies with: the language when
the output is idiomatic to one language (`swift`, `python`, `kotlin`), and
the runtime when several languages consume it equally or two targets share a
language (`dotnet` serves C#, F#, and Visual Basic; `node` and `wasm` both
emit JavaScript). A deployment platform is never a target name: `kotlin`
covers Android and the desktop JVM, selected by a setting. The same token
names the `--target` value, the `[generators.<target>]` table, the output
directory, and the page under [Generators](../generators/README.md).
