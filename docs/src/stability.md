# Stability and Versioning

WeaveFFI follows [Semantic Versioning](https://semver.org/) once it reaches
1.0.0. Until then it is in active pre-1.0 development and **any** surface area
may change between minor versions. This page documents exactly what is and
isn't covered, what the deprecation policy will look like post-1.0, and how
to bind your CI to a stable WeaveFFI workflow today.

## What semver covers (post-1.0)

After the 1.0.0 release, the following surfaces will be governed by SemVer:

- **CLI flags and subcommands.** Every documented `weaveffi <subcommand>`,
  every flag, every exit code, and every documented stdout/stderr format
  (`--format json` payloads in particular). Adding a new optional flag is a
  minor bump; removing or renaming one is a breaking change.
- **IDL schema.** The set of accepted top-level keys, type-reference syntax
  (`iter<T>`, `[T]`, `{K:V}`, `T?`, primitives, and user-defined struct, enum,
  interface, and callback-interface names), `version` semantics, and the
  JSON Schema exported by `weaveffi schema --format json-schema`.
- **Generated code shape.** The exported symbol names, function signatures,
  type names, package layouts, and ABI conventions of every generator's
  output. A patch release will not change the bytes of the generated output;
  a minor release may add new symbols but will not remove or rename existing
  ones; a major release may break.
- **Public Rust API of every published crate.** That is `weaveffi-ir`,
  `weaveffi-abi`, `weaveffi-core`, `weaveffi-gen-c`, `weaveffi-gen-cpp`,
  `weaveffi-gen-swift`, `weaveffi-gen-kotlin`, `weaveffi-gen-node`,
  `weaveffi-gen-wasm`, `weaveffi-gen-python`, `weaveffi-gen-dotnet`,
  `weaveffi-gen-dart`, `weaveffi-gen-go`, `weaveffi-gen-ruby`, and
  `weaveffi-cli`. The `LanguageBackend` trait, the `Orchestrator`, the IR
  types, and the C ABI runtime symbols exported from `weaveffi-abi` are all
  public contracts.
- **The C ABI revision.** The `weaveffi_abi_version()` runtime symbol
  reports the revision of the runtime surface (the `weaveffi_error` layout,
  the value-buffer encoding, the object and callback-interface conventions,
  and the `weaveffi_*` symbol set); the current revision is 2, and
  [C ABI Contract](reference/abi.md) is its normative description. Generated
  consumers embed the revision they were built for and, where a load-time
  check is cheap (Python, Ruby, Dart, Go, .NET, Node.js, Wasm), refuse to
  run against a producer reporting a different one. Post-1.0, the revision
  only changes with a major release.

## What is NOT covered pre-1.0

While the workspace is at `0.x`, **everything** above may change without
warning. In practice we try to keep breaking changes batched (one batch per
minor release, with a schema-version bump), but the contract is "no
contract." Things that have already changed during 0.x:

- IR type-reference syntax (`callback` was removed in `0.3.0`).
- The IR `TypeRef::Struct` variant was split into `Named` (the
  unresolved parsed form), `Record`, and `RichEnum`, and a shared
  marshalling-plan module (`weaveffi_core::plan`) now states the
  calling contracts every generator renders. Generated wrapper
  surfaces changed with it: `iter<T>` returns became each target's
  native lazy iteration idiom (Go `[]T` became an `iter.Seq`, C++
  `std::vector<T>` became a range type), and async result buffers
  became producer-freed after the completion callback returns.
- Schema `0.5.0` introduced first-class interfaces (`interfaces:`, objects
  with constructors, methods, and statics) and per-function typed errors
  (`throws:`), and made bare type names unique across the whole API. The
  samples' handle-based resource surfaces were rewritten as interfaces.
- Schema `0.7.0` moved records, rich enums, optionals, lists, and maps to
  by-value crossing as serialized value buffers (one
  `(const uint8_t*, size_t)` pair per value; see the
  [Value Buffer Protocol](reference/value-buffers.md)), replacing opaque
  record pointers, per-type create/destroy/getter symbols, and parallel-array
  maps. Nested composites became fully supported. The struct `builder:` flag
  and the `#[weaveffi::builder]` attribute were removed. Error codes gained
  structured payload `fields:`, and `weaveffi_error` gained
  `payload_ptr`/`payload_len` slots.
- The `Generator` trait gained `generate_with_config` in `0.3.0`, then
  was reworked in `0.5.0` into an associated `Config` type (with an
  object-safe `DynGenerator` view) that replaced the
  `*_with_config` method pair. A prototype Tera template hook
  (`generate_with_templates`, `--templates`, `template_dir`) was added
  and then removed in `0.4.0` because no generator ever consumed it.
  Schema `0.8.0` folded `Generator` into the single `LanguageBackend`
  trait (`Target` is its object-safe view), and generators now consume a
  fully resolved `Ty` type model rather than matching on the IDL's
  `TypeRef` strings.
- The C ABI runtime added `weaveffi_arena_*` and `weaveffi_cancel_token_*`
  families, and then `weaveffi_abi_version`.
- Schema `0.8.0` removed the top-level `package:` and `generators:` blocks
  from the IDL. Package identity and per-target options moved to a
  `weaveffi.toml` next to the definition (auto-discovered, or named with
  `--config`), and unknown top-level IDL keys are now rejected. The
  peripheral `new`, `scaffold`, `watch`, `format`, `lint`, `doctor`, and
  `man` CLI commands were removed; `validate --warn` reports the former
  lint warnings.
- Schema `0.9.0` and **ABI revision 2** made interface objects reference
  counted (every interface gained `_clone` beside `_destroy`, and objects may
  now appear inside records, optionals, lists, maps, iterators, and async
  results as `u64` tokens carrying one strong reference), replaced
  module-level `callbacks:`/`listeners:` with `callback_interfaces:` (a
  consumer-implemented vtable of methods), made the async executor pluggable
  (`weaveffi::set_spawner`), and added `weaveffi_error_set` plus the
  `FOREIGN_ERROR_CODE` (`-4`) reserved code. It removed the `handle`,
  `handle<T>`, `&str`, and `&[u8]` type spellings, `Param.mutable`,
  `Function.since`, the `weaveffi_arena_*` runtime, and `weaveffi::Handle`,
  and renamed the `android` target to `kotlin`. See the
  [migration guide](#migrating-from-schema-080--abi-1-to-090--abi-2) below.

Pin the WeaveFFI version in CI (`cargo install weaveffi-cli --version
=0.22.0`) and vendor the generated output in your repository so that
upgrades are an explicit, reviewable event.

## Migrating from schema 0.8.0 / ABI 1 to 0.9.0 / ABI 2

This release removed constructs rather than deprecating them, so a `0.8.0`
document is rejected until it's rewritten. Change `version: "0.8.0"` to
`version: "0.9.0"`, then work through the tables below. `weaveffi validate`
names every remaining offender with a hint; a Rust producer gets the same
guidance as compile errors from the macros.

### Removed IDL constructs

| Removed in `0.8.0`                        | Replacement in `0.9.0`                                                                                                  |
|-------------------------------------------|-------------------------------------------------------------------------------------------------------------------------|
| `type: handle` (untyped opaque token)     | Declare an interface for the resource and use its name as the type. Every interface is reference counted and may appear anywhere a type can. |
| `type: handle<T>`                         | `type: T`, where `T` is an interface declared under `interfaces:`.                                                         |
| `type: "&str"` / `type: "&[u8]"`          | `type: string` / `type: bytes`. Parameters of these families are always borrowed views at the ABI, so the explicit borrowed spelling no longer means anything. |
| `mutable: true` on a parameter            | Drop it. Interface methods take a shared `&self`; guard interior state with a `Mutex` (or another `Sync` primitive) in the producer. |
| `since:` on a function                    | Drop it. Use `deprecated: "<message>"` to mark surface that's on its way out; generators emit the target's deprecation marker. |
| module-level `callbacks:`                 | A `callback_interfaces:` entry whose `methods:` list the calls the producer makes. Functions that took a callback type take the callback interface by name instead. |
| module-level `listeners:` (`register_*` / `unregister_*` returning a subscription id) | A `callback_interfaces:` entry plus an ordinary interface method (or function) that accepts it, such as `subscribe(listener: MyListener)`. The producer retains the implementation for as long as it likes; the consumer's `free` fires when the last reference drops, so there's no unregister token to manage. |

A callback-interface method is synchronous, can't declare `throws`, `async`,
or `cancellable`, and returns nothing, a scalar, `bool`, or a C-style enum.
Its parameters may use any type except another callback interface or an
iterator. Callback interfaces may appear only as parameters of functions,
constructors, statics, and methods (not as returns, not inside records or
collections, and not as `T?`).

### Renamed things

| `0.8.0` / ABI 1                              | `0.9.0` / ABI 2                                                        |
|----------------------------------------------|------------------------------------------------------------------------|
| `--target android`, output dir `android/`    | `--target kotlin`, output dir `kotlin/` (with `build.gradle.kts`)       |
| `[generators.android]` in `weaveffi.toml`    | `[generators.kotlin]`                                                  |
| crate `weaveffi-gen-android`                 | crate `weaveffi-gen-kotlin`                                            |
| `#[weaveffi::callback]` / `#[weaveffi::listener]` | `#[weaveffi::callback_interface]` on a `trait Name: Send + Sync`   |
| `weaveffi::Handle`                           | `Arc<T>` of a `#[weaveffi::interface]` type                            |
| `weaveffi_arena_*` runtime symbols           | Removed. Buffered values are individually owned and freed with `weaveffi_free_bytes`. |
| `errors::ResolvedError`, `ConstPos::East` (Rust API) | Removed.                                                       |
| `ABI_VERSION == 1`                           | `ABI_VERSION == 2`                                                     |

The reserved error codes are now `GENERIC_ERROR_CODE = -1`,
`PANIC_ERROR_CODE = -2`, `MARSHAL_ERROR_CODE = -3`, and the new
`FOREIGN_ERROR_CODE = -4` (a consumer callback-interface implementation
raised). Consumers report the last one through the new runtime symbol
`weaveffi_error_set(err, code, message)` so no foreign allocator touches
`weaveffi_error.message`.

### Rust producer changes

- Every `#[weaveffi::interface]` type must be `Send + Sync`; the macro
  asserts it. Constructors return `Self`, `Arc<Self>`, or a `Result` of
  either; methods take `&self` or `self: Arc<Self>`. `&mut self` methods no
  longer compile.
- Interface types can be used as `Arc<T>` (or `&T` for a borrowed parameter)
  in any position: `Option<Arc<T>>`, `Vec<Arc<T>>`, `BTreeMap<K, Arc<T>>`,
  record fields, iterator items, and async returns.
- Replace `#[weaveffi::callback]`/`#[weaveffi::listener]` with a
  `#[weaveffi::callback_interface] trait Name: Send + Sync { ... }` and accept
  it as `Arc<dyn Name>`. Treat every call into it as potentially panicking
  (a consumer failure unwinds to the exported thunk, which reports
  `FOREIGN_ERROR_CODE`): snapshot state and release locks before calling out.
- Async exports run on the installed spawner. Nothing changes if the default
  thread-per-future executor is fine; call `weaveffi::set_spawner(...)` once
  at startup to run futures on Tokio or another runtime instead.

### Consumer-facing ownership changes

- **Every wrapper owns one strong reference.** Wrappers release it through
  the language's disposal hook (`close()`, `Dispose()`, `deinit`, RAII
  destructor, `dispose()`, `Close()`) with a garbage-collector backstop where
  one exists. Two wrappers may refer to the same native object; releasing one
  never invalidates the other.
- **Objects returned from calls, iterators, async results, and callback-method
  parameters are adopted.** Consumers no longer need to think about whether a
  returned pointer is borrowed: it's always one reference they own.
- **Objects inside value buffers are tokens carrying one reference.** A
  wrapper that encodes an object into a record or list calls `_clone` first;
  a buffer that contains objects is decoded exactly once.
- **Callback implementations are freed by the producer.** The producer calls
  the vtable's `free(ctx)` exactly once when its last reference drops; there
  is no unregister call. A consumer implementation that raises is reported to
  the original caller as `FOREIGN_ERROR_CODE`.
- **Kotlin consumers move from `android/` to `kotlin/`** and from a Groovy
  `build.gradle` to `build.gradle.kts`. The generated code still targets
  Android (JNI plus Gradle) and also runs on the desktop JVM.
- **Node.js consumers receive `i64`/`u64` values as `BigInt`**, and Dart
  wrappers use `NativeFinalizer` as their backstop.

## Post-1.0 deprecation policy

Once we reach 1.0.0, breaking changes will follow this path:

1. The feature is marked deprecated in a minor release. The CLI prints a
   `--warn`-style diagnostic (`weaveffi: warning: <name> is deprecated;
   <suggested replacement>`) on every invocation that touches it. The
   generators emit a native deprecation marker where the target language
   supports one (`#[deprecated]` in Rust, `@Deprecated` in Kotlin/Java,
   `@available(*, deprecated:)` in Swift, `[Obsolete]` in .NET, JSDoc
   `@deprecated` in TypeScript, and so on, driven by the existing IDL
   `deprecated:` field).
2. The deprecated feature continues to work for **at least one full minor
   version**.
3. Removal lands in the next major release with a migration note in
   `CHANGELOG.md`.

In short: nothing disappears in a patch release, nothing disappears without
at least one minor release of warnings, and every removal ships with a
documented replacement.

## IR schema version policy

The IR schema version is independent of the workspace version, but it is
tied to `weaveffi-ir`'s minor version: each `weaveffi-ir` minor bump
corresponds to at most one schema version bump.
[`CURRENT_SCHEMA_VERSION`](https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-ir/src/ir.rs)
in `crates/weaveffi-ir/src/ir.rs` is the source of truth; the current
schema version is `0.9.0`.

Pre-1.0, **only the current schema version is accepted**
(`SUPPORTED_VERSIONS` contains exactly `CURRENT_SCHEMA_VERSION`), so a
document declaring any earlier revision is rejected with an actionable
error. When a schema bump lands, update the `version` field in your IDL and
adjust the document to the new schema by hand; the changes are documented
in `CHANGELOG.md` and, for the `0.9.0` bump, in the
[migration guide](#migrating-from-schema-080--abi-1-to-090--abi-2) above.
Post-1.0, schema bumps will ship with an automated migration tool and a
widened `SUPPORTED_VERSIONS` window.

The C ABI revision is independent of both: it changes only when the runtime
contract changes incompatibly. Schema `0.9.0` happened to ship with ABI
revision 2 because reference-counted objects and callback-interface vtables
needed new symbols; a future schema bump that only adds IDL surface would
leave the ABI revision alone.

## Generated-code stability (determinism)

> **Regenerating with the same WeaveFFI version on the same IDL produces
> byte-identical output.**

This is enforced by the determinism tests: every generator's output is
hashed and re-hashed on the kitchen-sink fixture, and any deviation fails
CI. Internally, every
`HashMap` iteration that contributes to generated output has been replaced
by `BTreeMap` or an explicit sort. The `serde_json`-backed cache key uses a
canonical key ordering.

Practical consequences:

- Vendoring the generated `bindings/` directory in your repository is
  safe. A reviewer will only see a diff when the IDL or the generator
  itself changes.
- `weaveffi diff --check` (see below) is a reliable CI gate.
- Cross-platform regeneration (Linux vs macOS vs Windows) produces the
  same bytes for the same WeaveFFI version.

If you ever observe non-determinism, please file an issue with the IDL
that triggers it. It's a bug, not a quirk.

## The `weaveffi diff --check` workflow for downstream CI

The single recommended way to guard a downstream repository against
"forgot to regenerate" mistakes is `weaveffi diff --check`:

```bash
weaveffi diff path/to/api.yml --out generated/ --check
```

`diff --check` regenerates into a temporary directory, compares against
`--out`, and exits:

- **0** when the on-disk output matches what regeneration would produce,
- **2** when at least one file differs (modified content),
- **3** when files are missing or extra (a target was added/removed).

It prints only the summary `+ N added, - M removed, ~ K modified`,
suitable for CI logs without flooding the output.

A typical GitHub Actions step:

```yaml
- name: Verify generated bindings are up to date
  run: |
    cargo install weaveffi-cli --locked --version =0.22.0
    weaveffi diff idl/api.yml --out generated/ --check
```

Combine it with `weaveffi validate --warn idl/api.yml` (schema correctness
plus advisory lints) for a complete CI guard.

## See also

- [IDL Schema](reference/idl.md): the type system the schema version
  governs.
- [C ABI Contract](reference/abi.md): the normative description of ABI
  revision 2.
- [Roadmap](roadmap.md): what's planned after ABI 2 and the criteria for 1.0.
- [Getting Started](getting-started.md): installation and the basic
  workflow `diff --check` plugs into.
