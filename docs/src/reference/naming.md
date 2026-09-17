# Naming and Package Conventions

This page has two halves. The first is the naming policy for the WeaveFFI
project itself: brand names, repository slugs, and package names across
registries. The second is the naming policy the **generators** apply to
the identifiers they emit from your IDL: C symbols, wrapper classes,
functions, error types, and the Kotlin package. Every rule in the second
half was checked against the output of `weaveffi generate` for the
`kvstore` and `events` samples.

## Project names

### Human-facing brand names (prose)

Use the condensed names in sentences and documentation: WeaveFFI,
WeaveHeap. The brand stem is spelled `WeaveFFI` with an uppercase `FFI`
everywhere, including in generated code (`WeaveFFIError`,
`WeaveFFIException`), never the case-converter's `Weaveffi`.

### Repository and package slugs (URLs and registries)

- Use condensed lowercase slugs for top-level repositories: GitHub
  `weavefoundry/weaveffi`, `weavefoundry/weaveheap`.
- Use hyphenated slugs for subpackages and components, prefixed with the
  top-level slug: `weaveffi-core`, `weaveffi-ir`, `weaveffi-gen-kotlin`.
- Planned package names (not yet published): crates.io `weaveffi`,
  `weaveffi-core`, `weaveffi-ir`, and so on; npm `@weavefoundry/weaveffi`;
  PyPI `weaveffi`; SPM repo slug `weaveffi`.

Rationale: condensed top-level slugs unify handles across registries and
are ergonomic to type; hyphenated subpackages remain idiomatic and map
cleanly to ecosystems that normalize to underscores or CamelCase.

### Code identifiers by ecosystem

- Rust: hyphenated subcrates on crates.io (`weaveffi-core`), imported with
  underscores (`weaveffi_core`); the facade crate is `weaveffi`. Modules
  and paths are snake_case; types, traits, and enums are CamelCase.
- Swift: package products and modules are UpperCamelCase (`WeaveFFI`);
  the repo slug stays condensed and the SPM product provides the CamelCase
  surface.
- Java and Kotlin: group ID and package base are reverse-DNS lowercase
  (`com.weavefoundry.weaveffi`); artifact IDs are condensed at the top level
  (`weaveffi`) and hyphenated for sub-artifacts (`weaveffi-android`); class
  names are UpperCamelCase (`WeaveFFI`).
- JavaScript and TypeScript: scope plus condensed name for the top-level
  package (`@weavefoundry/weaveffi`), hyphenated for subpackages
  (`@weavefoundry/weaveffi-core`).
- Python: PyPI name condensed at the top level (`weaveffi`), hyphenated for
  subpackages (`weaveffi-core`); import as `weaveffi` and `weaveffi_core`.
- C and CMake: target and library names are snake_case (`weaveffi`,
  `weaveffi_core`); includes are directory based (`#include <weaveffi/weaveffi.h>`).

### Writing guidelines

- In prose, prefer the condensed brand names.
- In code snippets, follow the host language conventions above.
- For cross-language docs, show both the repo or package slug and the
  language-appropriate identifier on first mention: "Install `weaveffi`
  (import as `weaveffi`, Swift module `WeaveFFI`)."
- New crates and packages follow the condensed-top-level plus
  hyphenated-subpackage pattern (`weaveffi-*`, `weaveheap-*`). Avoid
  hyphenated top-level slugs such as `weave-ffi`.

## Generated identifiers

The generators derive every emitted name from three inputs: the IDL name,
the module path, and the target's casing idiom. The rules below are what
the shared model in `weaveffi-core` decides once for all targets
(`weaveffi_core::utils`, `weaveffi_core::errors`, and the ABI slot
assignment in `weaveffi_core::model`), followed by each target's casing.
IDL names are expected to be `snake_case` for modules, functions,
parameters, and fields and `PascalCase` for types, variants, and error
codes; the validator only enforces that they are identifiers, so a name
written in another style is re-cased on a best-effort basis.

### C ABI symbols

The C names are normative in the [C ABI Contract](abi.md); this is the
summary. `{prefix}` is `weaveffi` unless a project sets `c_prefix`, and
`{path}` is the module path joined with underscores (`kv`, `kv_stats`).

| Declaration                          | C spelling                                   | `kvstore` example                                 |
|--------------------------------------|----------------------------------------------|---------------------------------------------------|
| free function `f` in module `{path}` | `{prefix}_{path}_f`                          | `weaveffi_kv_stats_get_stats`                     |
| interface `T` (opaque tag)           | `{prefix}_{path}_T`                          | `weaveffi_kv_Store`                               |
| interface member `m` (ctor, method, static) | `{tag}_m`                             | `weaveffi_kv_Store_open`, `weaveffi_kv_Store_get` |
| implicit lifecycle                   | `{tag}_clone`, `{tag}_destroy`               | `weaveffi_kv_Store_clone`, `weaveffi_kv_Store_destroy` |
| callback interface `L` (vtable type) | `{prefix}_{path}_L_vtable`                   | `weaveffi_kv_EvictionListener_vtable`             |
| callback parameter `p`               | `void* p_ctx, const {vtable}* p_vtable`      | `listener_ctx`, `listener_vtable`                 |
| C-style enum `E` and variant `V`     | `{prefix}_{path}_E`, `{prefix}_{path}_E_V`   | `weaveffi_kv_EntryKind_Volatile`                  |
| error domain `D` and code `C`        | `{prefix}_{path}_D` enum, `{prefix}_{path}_D_C` | `weaveffi_kv_KvError_KeyNotFound`              |
| iterator returned by `f`             | `{owner}_{PascalF}Iterator` tag with `_next` and `_destroy` | `weaveffi_kv_Store_ListKeysIterator` |
| async function `f`                   | `{sym}_async` launcher, `{sym}_callback` typedef | `weaveffi_kv_Store_compact_async`             |
| `bytes` or buffered parameter `p`    | `p_ptr`, `p_len`                             | `value_ptr`, `value_len`                          |
| out-length of a returned buffer      | `out_len`                                    |                                                   |
| error out-parameter                  | `out_err`                                    |                                                   |

The iterator `{owner}` is `{prefix}_{path}` for a free function and
`{prefix}_{path}_T` for a method or static of interface `T`; the function
name is re-cased to PascalCase (`list_keys` becomes `ListKeys`). Types are
never re-cased: the IDL spelling of `Store`, `EntryKind`, and `KeyNotFound`
appears verbatim in every C symbol. A cross-module type reference resolves
to its owner's path, so a `stats` function taking the parent module's
`Store` still spells the parameter `const weaveffi_kv_Store*`.

Runtime symbols (`weaveffi_error`, `weaveffi_error_set`,
`weaveffi_error_clear`, `weaveffi_error_free`, `weaveffi_free_string`,
`weaveffi_free_bytes`, `weaveffi_abi_version`, and the four
`weaveffi_cancel_token_*` functions) always keep the `weaveffi_` spelling
in the producer. Under a non-default prefix the generated header adds
`#define {prefix}_{name} weaveffi_{name}` aliases for each of them, so
consumer code may use either spelling.

Because free functions and interface members share `{prefix}_{path}_`, a
free function named `Store_get` next to a `get` method on `Store` is a
validation error (`AbiSymbolCollision`), as is a free function named
`Store_clone` or `Store_destroy`.

### Functions, methods, and parameters

Free functions, methods, statics, constructors, and parameters are re-cased
to the target's idiom:

| Target        | Callables and parameters | Example (`get_stats`, `open_many`)              |
|---------------|--------------------------|------------------------------------------------|
| C, C++, Python, Ruby | `snake_case` (unchanged) | `get_stats`, `open_many`                 |
| Swift, Kotlin, Dart, Node, Wasm | `camelCase`   | `getStats`, `openMany`                          |
| Go, .NET      | `PascalCase`             | `GetStats`, `OpenMany`                           |

A free function keeps its bare name by default. Every target except `c`,
`cpp`, and `wasm` has a `strip_module_prefix` option (see
[Project Configuration](../guides/config.md)); setting it to `false`
prepends `{module}_` before re-casing, giving `kv_stats_get_stats`,
`kvStatsGetStats`, or `KvStatsGetStats`.

Where a free function lives depends on the target's notion of a
namespace. Python, Ruby, C++, Dart, Node, and Wasm emit module-level
functions (Ruby's live on the package module, `Kvstore.get_stats`). Swift
nests them in one `enum` per module path segment (`Kv.Stats.getStats`).
.NET puts them on a static class per module whose name is the PascalCase
module path (`Kv.GetStats`, `KvStats.GetStats`). Kotlin puts every free
function on the companion object of a single `WeaveFFI` holder class
(`WeaveFFI.getStats(...)`). Go exports them at package level.

Constructors follow the interface: a constructor named `new` becomes the
canonical constructor where the target has one (Swift `init`, Python
`__init__`, Ruby `initialize`, a C# or C++ constructor, a Dart `factory
EventBus()`, JavaScript `new EventBus()`, and in Kotlin a companion
`operator fun invoke()` so `EventBus()` reads like a constructor); every
other constructor becomes a static factory in the target's casing
(`Store.open`, `Store.openMany`, `Store.Open`). Go has no constructors, so
each becomes a package-level function named `{PascalCtor}{Type}`
(`NewEventBus`, `OpenStore`); Dart uses a named constructor
(`Store.open`).

### Records and enums

Struct and rich-enum names are kept verbatim (`Entry`, `StoreInfo`,
`Shape`) in every target; each is an idiomatic value type (a Kotlin `data
class`, a Swift `struct`, a Go `struct`, a .NET `sealed class`, a
TypeScript `interface`, a Python class with annotated attributes).
Fields are re-cased like parameters (`created_at` becomes `createdAt` in
Swift, Kotlin, Dart, and JavaScript and `CreatedAt` in Go and .NET).

C-style enum names are kept verbatim. Variant casing follows each target's
enum idiom:

| Target                                  | Variant of `EntryKind { Volatile }` |
|-----------------------------------------|-------------------------------------|
| C, C++, Kotlin, Python, .NET, Node, Wasm, Dart | `Volatile`                    |
| Swift                                   | `.volatile` (lowerCamelCase case)   |
| Go                                      | `EntryKindVolatile` (type-prefixed constant) |
| Ruby                                    | `EntryKind::VOLATILE` (module constant) |

### Interfaces

An interface `Store` is a class named `Store` in every target (a Swift
`final class`, a Kotlin `class ... : AutoCloseable`, a Python class, a Ruby
class over an `FFI::AutoPointer`, a Go `struct` used through `*Store`, a
.NET `class Store : IDisposable`, a Dart `class Store implements
Finalizable`, a C++ RAII class with copy semantics that call `_clone`, a
TypeScript class). Methods and statics are re-cased like free functions
and attached to the class; statics use the target's static idiom
(`static func`, `companion object`, `@staticmethod`, `def self.`, C#
`static`, and package-level functions in Go).

The wrapper's disposal method is named by the target's convention, never
by the IDL: `close()` in Kotlin, Python, Dart, Node, and Wasm (also
`Symbol.dispose` in JavaScript and `AutoCloseable` in Kotlin), `Close()`
in Go, `Dispose()` in .NET, and the destructor or `deinit` in C++ and
Swift. Ruby releases through the `AutoPointer` and exposes `close`. No
target exposes `clone`; copying a wrapper (C++ copy constructor, or simply
holding two references) is the only way to get a second reference from the
consumer side.

### Callback interfaces

A callback interface `EvictionListener` becomes the target's "abstract
method set" type, named after the IDL with at most one idiomatic prefix:

| Target   | Declaration                                              |
|----------|----------------------------------------------------------|
| C        | `weaveffi_kv_EvictionListener_vtable` struct             |
| C++      | `class EvictionListener` with pure virtual methods       |
| Swift    | `protocol EvictionListener`                              |
| Kotlin   | `interface EvictionListener`                             |
| Python   | `class EvictionListener(abc.ABC)`                        |
| Ruby     | `module EvictionListener` (mixin with `NotImplementedError` stubs) |
| Go       | `type EvictionListener interface`                        |
| .NET     | `interface IEvictionListener` (C#'s `I` prefix)          |
| Dart     | `abstract class EvictionListener`                        |
| Node, Wasm | `interface EvictionListener` in the `.d.ts`            |

Methods are re-cased like other callables (`on_evict` becomes `onEvict`,
`OnEvict`, or stays `on_evict`). No lifecycle name is exposed to the
consumer: the vtable's trailing `free` entry and the `ctx` handle are
generated glue, and the consumer only ever hands over an implementation.

### Error domains and codes

The base error type is branded, and every target uses one of two brands
from `weaveffi_core::errors`: `WeaveFFIError` where the ecosystem's errors
end in `Error` (Swift, Python, TypeScript, C++, Go) and
`WeaveFFIException` where they end in `Exception` (Kotlin, .NET, Dart).
Ruby is the one exception: its base class is `Error` nested in the package
module (`Kvstore::Error`), because the module already provides the brand.

A domain `KvError` with codes `KeyNotFound` and `IoError` yields:

| Target  | Domain type                      | Per-code name                                    |
|---------|----------------------------------|--------------------------------------------------|
| C, C++ header enum | `weaveffi_kv_KvError` | `weaveffi_kv_KvError_KeyNotFound` constants           |
| C++     | `class KvError : WeaveFFIError`  | `class KeyNotFoundError : KvError`, `class IoError`  |
| Swift   | `enum KvError: Error`            | `case keyNotFound(message:)`, `case ioError(message:)` |
| Kotlin  | `sealed class KvException : WeaveFFIException` | nested `KvException.KeyNotFound`, `KvException.IoError` |
| Python  | `class KvError(WeaveFFIError)`   | `class KeyNotFound(KvError)`, also reachable as `KvError.KeyNotFound` |
| Ruby    | `class KvError < Kvstore::Error` | nested `KvError::KeyNotFound`, `KvError::IoError`    |
| Go      | `type KvError struct`            | `KvErrorKeyNotFound` and `KvErrorIoError` code constants |
| .NET    | `class KvException : WeaveFFIException` | `KvException.KeyNotFound` code constants        |
| Dart    | `class KvException extends WeaveFFIException` | `class KeyNotFoundException`, `class IoException` |
| Node, Wasm | `class KvError extends WeaveFFIError` | `class KeyNotFoundError`, `class IoError`     |

The rules behind the table:

- `type_name(raw, suffix)` re-cases a name to PascalCase and appends the
  suffix exactly once: `KEY_NOT_FOUND` with `Error` is `KeyNotFoundError`,
  and `AlreadyError` stays `AlreadyError`. C++, Node, and Wasm use it with
  `Error`; Dart uses it with `Exception`.
- `exception_type_name(raw)` swaps a trailing `Error` stem for `Exception`
  instead of stacking them: `KvError` becomes `KvException` (Kotlin, .NET,
  Dart), `Failure` becomes `FailureException`, and a domain named just
  `Error` falls back to the brand.
- `pascal(raw)` is the suffix-free PascalCase form used where codes are
  nested cases or constants (Kotlin, Ruby, Python, .NET, Go).
- Swift lowerCamelCases the code into an enum case (`keyNotFound`).

The upshot for IDL authors: write PascalCase code names and let the
generator add or replace suffixes. Writing `KeyNotFoundError` as the code
name produces `KeyNotFoundError` in C++ and `KeyNotFoundException` in Dart
(the stem is normalized), but `KeyNotFoundError` as the Kotlin nested class
and `keyNotFoundError` as the Swift case, which is why the samples leave
the suffix off. Code names are unique across the whole API for exactly this
reason: every target flattens them into one namespace.

### Package, module, and file names

Ecosystem package names come from `[package] name` in `weaveffi.toml`
(`kvstore` in the samples) and are re-cased per ecosystem; see
[Project Configuration](../guides/config.md#package). The names that
appear in code:

| Target  | Namespace derived from the package name                              |
|---------|----------------------------------------------------------------------|
| Swift   | module `Kvstore` (PascalCase); free functions under per-module enums  |
| Ruby    | `module Kvstore` wrapping everything                                 |
| .NET    | `namespace Kvstore`                                                  |
| C++     | `namespace kvstore` (lowercase)                                      |
| Go      | `package kvstore`; the import path comes from `[generators.go] module_path` |
| Python  | package directory `kvstore/` containing `weaveffi.py` and `weaveffi.pyi` |
| Kotlin  | Gradle `rootProject.name = "kvstore"`; the JVM package comes from config |

### Kotlin package and class names

The `kotlin` target is configured under `[generators.kotlin]`:

```toml
[generators.kotlin]
package = "com.example.kv"      # default "com.weaveffi"
strip_module_prefix = true      # default
```

- **Package.** Every generated Kotlin file starts with `package {package}`
  and is written under `src/main/kotlin/{package with dots as slashes}/`,
  so the default lands at `src/main/kotlin/com/weaveffi/WeaveFFI.kt`. The
  JNI glue is `src/main/cpp/weaveffi_jni.c`, and its exported JNI symbols
  are derived from the package (`Java_com_weaveffi_WeaveFFI_...`), so
  changing the package regenerates both files consistently.
- **Holder class.** Free functions live on the companion object of
  `class WeaveFFI`, together with `setCallbackExceptionHandler`. The
  holder's name is fixed; only the package is configurable.
- **Types.** Interfaces, records, enums, callback interfaces, and errors
  are top-level declarations in the same package, named as in the tables
  above (`Store`, `Entry`, `EntryKind`, `EvictionListener`, `KvException`).
  An iterator returned by `list_keys` on `Store` is
  `KvStoreListKeysIterator` (`{PascalModule}{Interface}{PascalFn}Iterator`),
  implementing `Iterator<T>` and `AutoCloseable`.
- **Runtime glue.** Internal helpers are prefixed `Weave` or `weave`
  (`WeaveNativeLibrary`, `WeaveBufferReader`, `weaveCleaner`) and marked
  `internal`, so they never collide with an IDL type whose name starts
  with `WeaveFFI`.
