# WeaveFFI — Product Requirements Document v4 (1.0 Production Readiness)

WeaveFFI is a Rust CLI that generates multi-language FFI bindings from a single
API definition file (YAML/JSON/TOML). It targets C, C++, Swift, Android/Kotlin,
Node.js, WASM, Python, .NET, Dart, Go, and Ruby. The generated code calls into
a user-written native library through a stable C ABI.

**Current state:** The CLI has eleven working generators, ~700 tests, five
samples (calculator, contacts, inventory, async-demo, events), annotated Rust
extraction, incremental codegen with caching, generator configuration via TOML,
inline IDL generators section, hook commands, Tera template scaffolding, schema
versioning, and automated publishing to crates.io via semantic-release. However
the project is **not yet production-ready**:

- The C generator emits `string` parameters as `const char*` while the Swift,
  Android, and WASM generators emit calls with a `(ptr, len, &err)` triple —
  the **same `string` parameter is represented incompatibly across targets**
  and any non-trivial function with a string param produces broken bindings.
- Generated Python, Dart, and .NET wrappers have **memory leaks** (Python never
  frees C-allocated string returns; Dart never frees `_calculator_echo` returns;
  .NET never calls `weaveffi_error_clear` on the error message pointer).
- `TypeRef::Callback` is in the IR and validator but **every generator panics
  with `todo!()`** if a callback type is used in a function signature, plus the
  scaffold generator and the Tera context builder.
- `listeners:` is emitted only by the C generator; the other ten generators
  silently ignore it, advertising a feature that doesn't work end-to-end.
- `Iterator` returns are handled correctly by C/Swift/Python but Node calls the
  iterator `_next` function with the wrong signature, Ruby treats them as Lists,
  and Go skips them.
- `Builder` is implemented as `withFoo` chains in Dart but `build()` throws
  `UnimplementedError`. Go's builder also stops at `With*` setters.
- `cancellable: true` async functions pass `NULL` for the cancel token in the
  Node generator (cancel token never reaches the native side).
- Async functions are silently skipped by the Go and Ruby generators.
- The `c_prefix` config option is wired to the C generator but every other
  generator hardcodes `weaveffi.h` / `libweaveffi.dylib` / `-lweaveffi`, so any
  user who customises `c_prefix` gets non-buildable bindings everywhere except C.
- `output_files` returns the default paths even when `swift_module_name`,
  `python_package_name`, `dotnet_namespace`, `ruby_gem_name`, or `android_package`
  is customised — `--dry-run` lies for those targets.
- `template_dir` is a defined `GeneratorConfig` field but no generator overrides
  `generate_with_templates`; user `.tera` files are loaded then ignored.
- `weaveffi diff` and `weaveffi doctor` always exit 0 — no CI gating possible.
- Cache hashing uses `serde_json::to_string` which is not stable across serde
  versions and HashMap orderings.
- Documentation has stale content: `docs/src/intro.md` lists 5 of 11 targets,
  `docs/src/samples.md` claims async is rejected (it isn't), `docs/src/reference/naming.md`
  says crates are unpublished (they are 0.2.x), `docs/src/getting-started.md`
  pins `weaveffi-abi = "0.1"`, and the canonical docs URL is inconsistent
  (`docs.weaveffi.com` vs `weavefoundry.github.io/weaveffi`).
- No examples exist for C++, Dart, Go, or Ruby; the WASM example is a stub
  HTML page; the Android example is a README pointing at the generated tree;
  the Node `contacts.mjs` example references symbols that the shipped node-addon
  does not expose.
- No samples have a README. The calculator sample has zero unit tests. There
  is no persistence-backed (SQLite/sled) or network-backed (HTTP) sample.
- The `weaveffi extract` command supports primitives, `String`, `Vec<u8>`,
  `Vec<T>`, `Option<T>`, `HashMap<K,V>`, structs, and enums, but not `&str`,
  `&[u8]`, `handle<T>`, `iter<T>`, callbacks, listeners, async, builders,
  deprecation, struct field defaults, or non-`i32`-repr enums.
- No SECURITY.md, no `cargo-audit`, no `cargo-deny`, no Dependabot, no CodeQL,
  no SBOM, no signed releases.
- CI declares no MSRV, runs no cross-compilation matrix, never executes the
  Criterion benchmarks, and on Windows only verifies that generator outputs
  exist (it does not compile or run the generated C / Node / Python code).
- The published artifact story is `cargo install weaveffi-cli` only — no
  prebuilt CLI binaries on GitHub Releases, no Homebrew tap, no Scoop bucket,
  no `.deb` / `.rpm` / `.msi`.

**Goal:** Deliver WeaveFFI 1.0. That means: every advertised feature works
end-to-end across every generator with parity tests proving it; every
generated artifact compiles, links, runs, and respects the stable C ABI
without leaks; the CLI provides a polished UX with structured exit codes,
JSON output, and source-position errors; the IR has a published, versioned,
stamped contract; the documentation is accurate, complete, and consistent;
samples and examples cover every target with at least one persistence- or
network-backed real-world demo; CI runs MSRV / cross-compilation / security /
benchmark gates; releases ship signed prebuilt binaries via Homebrew, Scoop,
`.deb`, `.rpm`, and `.msi` in addition to crates.io.

This PRD is intentionally large. We are pre-1.0 — **breaking changes to the
IR, the C ABI, the CLI surface, and the configuration file are explicitly
allowed and expected**. Tasks below assume `cargo test --workspace` passes
between phases; do not move to a later phase if earlier tasks are red.

---

## Tasks

### Phase 1 — String ABI unification (P0 correctness fix)

The C generator currently maps `TypeRef::StringUtf8` parameters to
`const char* name` in the C header, but the Swift, Android (JNI), and WASM
generators emit call sites with `(name_ptr, name_len, &err)`. Every
non-trivial function with a string parameter therefore produces bindings that
do not link. We unify the ABI by always representing string parameters as
`(const uint8_t* name_ptr, size_t name_len)` (raw byte pointer + length) in
the C header. This matches what is already emitted on the Swift/Android/WASM
side and is also more robust (no NUL-termination requirement, supports
arbitrary UTF-8 byte slices). String **return** values remain owned C strings
via `const char*` allocated by the callee and freed via `weaveffi_free_string`.

- [ ] Update the C generator string parameter ABI in `crates/weaveffi-gen-c/src/lib.rs`. In the helper that produces parameter type strings (find the function that maps `TypeRef::StringUtf8` for params; it is the same helper used for `Bytes`), change the `StringUtf8` parameter expansion from `const char* {name}` to `const uint8_t* {name}_ptr, size_t {name}_len`. Keep the `BorrowedStr` parameter mapping the same (it already uses pointer + length per Phase 11 of PRD-v3). Keep the **return** type for `StringUtf8` as `const char*`. In the C generator's struct accessor / setter functions and in the builder generation, also update string parameters consistently. Update the Map convention comment block at the top of the generated header to clarify: "String parameters are passed as `(const uint8_t* X_ptr, size_t X_len)` byte slices, not NUL-terminated. String returns are NUL-terminated `const char*` allocated by the callee and freed by the caller via `weaveffi_free_string`." Add tests `c_string_param_uses_ptr_and_len`, `c_string_return_uses_const_char_ptr`, `c_struct_string_field_setter_uses_ptr_and_len`, `c_builder_string_field_setter_uses_ptr_and_len`. Update every existing C generator test that asserted on the old `const char* X` parameter form to assert on the new pair form. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the C++ generator in `crates/weaveffi-gen-cpp/src/lib.rs` to match the new C ABI. The `extern "C" {}` block at the top of `weaveffi.hpp` mirrors the C header — it must declare every function with the new `(const uint8_t*, size_t)` string parameter pairs. The C++ wrapper functions that take `const std::string&` must call the raw C function with `reinterpret_cast<const uint8_t*>(s.data()), s.size()`. Update struct setters, builder setters, and async function wrappers similarly. Add tests `cpp_string_param_calls_raw_with_ptr_and_len`, `cpp_struct_setter_string_uses_ptr_and_len`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Node N-API addon in `crates/weaveffi-gen-node/src/lib.rs` to match the new C ABI. In `render_addon_c`, when extracting a `StringUtf8` parameter, after `napi_get_value_string_utf8` continue to allocate `s` of size `s_len + 1` and read the string, but pass `(const uint8_t*)s, (size_t)s_len` (NOT `s` alone) to the C function. For struct setter wrappers and builder setters, do the same. Update `node_addon_extracts_args` and any related tests. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Python generator in `crates/weaveffi-gen-python/src/lib.rs` to match. In `weaveffi.py`, for each function with a `StringUtf8` parameter, the `argtypes` entry must become `(ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t)` and the call must be: `_bytes = s.encode("utf-8"); _arr = (ctypes.c_uint8 * len(_bytes))(*_bytes); _result = _fn(_arr, len(_bytes), ...)`. Replace the existing `_string_to_bytes` helper with a `_string_to_byteslice(s: str) -> tuple` that returns `(arr, length)`. The struct setters and builder setters must call this helper. Update `weaveffi.pyi` to reflect that user-facing Python signatures still take `str` (the byteslice conversion is internal). Add a test `python_string_param_uses_ptr_and_len`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the .NET generator in `crates/weaveffi-gen-dotnet/src/lib.rs` to match. The P/Invoke declarations for any function with a `StringUtf8` parameter must become `(IntPtr {name}_ptr, UIntPtr {name}_len, ref WeaveffiError err)` and use `MarshalAs(UnmanagedType.LPArray, ArraySubType = UnmanagedType.U1)` or manual pinning via `GCHandle.Alloc(bytes, GCHandleType.Pinned)`. Add a `WeaveFFIHelpers.PinUtf8(string)` helper returning a `(GCHandle handle, IntPtr ptr, UIntPtr len)` triple, and the wrapper method must call it inside a `try { ... } finally { handle.Free(); }`. The struct setter and builder setter wrappers must use the same helper. Add a test `dotnet_string_param_uses_pinned_byteslice`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Dart generator in `crates/weaveffi-gen-dart/src/lib.rs` to match. The `typedef` for any function with a `StringUtf8` parameter must use `Pointer<Uint8>, IntPtr` (not `Pointer<Utf8>`) for the parameter pair. The Dart wrapper must convert the Dart `String` to a UTF-8 `Uint8List`, allocate via `pkg:ffi`'s `calloc<Uint8>(bytes.length)`, copy the bytes in, call the function with `(buf, bytes.length, err)`, and `calloc.free(buf)` in a `finally`. Update struct setters and builders. Add a test `dart_string_param_uses_uint8_pointer_and_length`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Go generator in `crates/weaveffi-gen-go/src/lib.rs` to match. For each function with a `StringUtf8` parameter, replace the current `C.CString(s)` + `defer C.free` pattern with: `bs := []byte(s); var p *C.uint8_t; if len(bs) > 0 { p = (*C.uint8_t)(unsafe.Pointer(&bs[0])) }; result := C.weaveffi_X((*C.uint8_t)(p), C.size_t(len(bs)), &cErr)`. Update struct setters and builders. Add a test `go_string_param_uses_byteslice_pointer_and_length`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Ruby generator in `crates/weaveffi-gen-ruby/src/lib.rs` to match. The `attach_function` types for a `StringUtf8` parameter must become `[:pointer, :size_t]` (pointer + length) instead of `:string`. The Ruby wrapper method must convert the Ruby string to bytes via `s.b` (binary encoding), allocate an `FFI::MemoryPointer.from_string(s.b)` (or `FFI::MemoryPointer.new(:uint8, s.bytesize, true)` and copy), call with `(buf, s.bytesize, err)`. Add a test `ruby_string_param_uses_pointer_and_length`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Swift generator in `crates/weaveffi-gen-swift/src/lib.rs` to verify its existing `(s_ptr, s_len, &err)` call pattern matches the new C signature exactly. The current code already passes pointer + length; verify the C system module imports the updated header so the function prototype resolves. Add a test `swift_string_param_uses_ptr_and_len_and_compiles_against_new_c_header`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Android JNI generator in `crates/weaveffi-gen-android/src/lib.rs` to verify its existing JNI bridge calls the new C signature with `(const uint8_t*)s_chars, (size_t)s_len, &err`. Add a test `android_jni_string_param_uses_ptr_and_len`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the WASM generator in `crates/weaveffi-gen-wasm/src/lib.rs` to verify its `_encodeString` helper produces `(ptr, len)` tuples that match the new C signature. The current generated JS already passes the tuple; verify the README reflects the new convention and remove any reference to NUL-terminated string params. Add a test `wasm_string_param_uses_ptr_and_len_in_js_call`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add an end-to-end string ABI parity test in `crates/weaveffi-cli/tests/`. Create `cli_string_abi_parity.rs` with a test `string_param_signature_consistent_across_generators`. Build a one-function API: `module: parity, fn echo(s: string) -> string`. Generate to a tmpdir for all 11 targets. Parse the resulting C header to extract the prototype of `weaveffi_parity_echo`. Then string-grep each target's wrapper file for the call site to `weaveffi_parity_echo` and assert the argument count matches the C declaration's arity. Specifically: count commas in the C function signature versus the Swift/Android/Python/.NET/etc. call site; they must match. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the contacts sample's Rust implementation in `samples/contacts/src/lib.rs` to accept the new string ABI. Every `#[no_mangle] pub extern "C" fn weaveffi_contacts_*` that currently takes `*const c_char` for a string parameter must be changed to `*const u8, usize`. Use `std::slice::from_raw_parts(ptr, len)` and `std::str::from_utf8(slice)` to convert. Update the inventory and async-demo samples similarly. Update the calculator sample's `weaveffi_calculator_echo` signature (which currently takes `*const c_char`) to take `*const u8, usize`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the scaffold generator in `crates/weaveffi-cli/src/scaffold.rs` to emit the new string parameter ABI. For each function param of type `StringUtf8` or `BorrowedStr`, generate `{name}_ptr: *const u8, {name}_len: usize` in the Rust skeleton. For struct setters and builder setters, do the same. Add a test `scaffold_string_param_emits_ptr_and_len`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 2 — Bytes ABI alignment and verification

Bytes parameters were already `(const uint8_t* X_ptr, size_t X_len)` per
existing convention but generator-side emitter inconsistencies have crept in
(some generators use `len`, others use `_len` suffix). Audit and unify.

- [ ] Audit every generator's handling of `TypeRef::Bytes` and `TypeRef::BorrowedBytes` parameters and returns. In each `crates/weaveffi-gen-*/src/lib.rs`, ensure the parameter expansion is exactly `(const uint8_t* {name}_ptr, size_t {name}_len)` for parameters and the return convention is `(uint8_t* out_ptr, size_t* out_len, weaveffi_error* out_err)` (out-pointers populated by callee, freed by caller via `weaveffi_free_bytes(ptr, len)`). For any generator that uses a different naming or shape, change it to match. Add a test in each generator named `{generator}_bytes_param_uses_canonical_shape` and `{generator}_bytes_return_uses_canonical_shape`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add an end-to-end bytes parity test. In `crates/weaveffi-cli/tests/cli_bytes_abi_parity.rs`, create a test `bytes_param_signature_consistent_across_generators` mirroring the string parity test but for a function `process(data: bytes) -> bytes`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 3 — Memory contract: error message lifetime

The `weaveffi_error` struct contains a `const char* message` allocated by the
Rust runtime. The convention is that `weaveffi_error_clear(&err)` frees the
message. The generated .NET wrapper currently captures the message via
`Marshal.PtrToStringUTF8(err.Message)` then throws — without calling
`weaveffi_error_clear`. The error message memory leaks on every error. Other
generators must be audited too.

- [ ] Fix the .NET error handling in `crates/weaveffi-gen-dotnet/src/lib.rs`. Update the generated `WeaveffiError.Check` method body to: read the code, marshal the message string, call `NativeMethods.weaveffi_error_clear(ref err)`, then throw. The `weaveffi_error_clear` P/Invoke must be declared in `NativeMethods`. Add a test `dotnet_error_check_calls_error_clear` that asserts the generated `WeaveffiError.Check` body contains `weaveffi_error_clear`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Audit the error-clear discipline in every generator. For each `crates/weaveffi-gen-*/src/lib.rs`, search the generated wrapper code for the error-handling block and verify it calls `weaveffi_error_clear` after capturing the message. Specifically check: C++ (`cpp_error_check_calls_error_clear` test), Swift (already calls `weaveffi_error_clear`; add a test), Android JNI (`throw_weaveffi_error` already calls it; add a test), Node N-API (already calls it; add a test), Python (`_check_error` already calls `_lib.weaveffi_error_clear`; add a test), Dart (already calls `_weaveffiErrorClear`; add a test), Go (already calls `C.weaveffi_error_clear`; add a test), Ruby (already calls `weaveffi_error_clear`; add a test), WASM JS (`_checkError` already calls `wasm.weaveffi_error_clear`; add a test). All of these tests should string-grep the rendered output for the clear call. Run `cargo test --workspace` to verify nothing is broken.

### Phase 4 — Memory contract: string return free discipline

The generated Python wrapper for any function returning `StringUtf8` reads
`ctypes.c_char_p` which immediately copies bytes into a Python `bytes` object,
losing the original pointer — there is no way to call `weaveffi_free_string`.
Result: every string-returning function leaks. The Dart wrapper similarly
calls `result.toDartString()` then `calloc.free(err)` but never frees the
returned C string. Both must be fixed.

- [ ] Fix Python string-return memory leak in `crates/weaveffi-gen-python/src/lib.rs`. Change the `restype` for any function returning `StringUtf8` from `ctypes.c_char_p` to `ctypes.POINTER(ctypes.c_char)` (raw pointer that does not auto-copy). Update the wrapper body to: `_ptr = _fn(...); _check_error(_err); if not _ptr: return ""; _s = ctypes.cast(_ptr, ctypes.c_char_p).value.decode("utf-8"); _lib.weaveffi_free_string(_ptr); return _s`. Apply the same pattern to struct getters, builder result getters, and async result delivery. Add a test `python_string_return_calls_free_string` that asserts the generated `weaveffi.py` for an `echo(s) -> string` function contains `_lib.weaveffi_free_string(_ptr)` after the cast/decode. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Fix Dart string-return memory leak in `crates/weaveffi-gen-dart/src/lib.rs`. The wrapper must call `weaveffi_free_string` on the returned `Pointer<Utf8>` after converting to a Dart `String`. Add a `_NativeWeaveffiFreeString` typedef and `_weaveffiFreeString` lookup at the top of the file. In the per-function wrapper body, change `return result.toDartString();` to `final str = result.cast<Char>() == nullptr ? '' : result.toDartString(); _weaveffiFreeString(result.cast<Char>()); return str;` (using `Pointer<Char>` for the `weaveffi_free_string(const char*)` signature). Apply the same pattern to struct getters, builder result getters, and async result delivery. Add a test `dart_string_return_calls_free_string`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add cross-generator string-return free discipline tests. Create `crates/weaveffi-cli/tests/string_return_free_audit.rs` with a test `every_generator_frees_returned_strings` that for each of the 11 targets generates code from a one-function API `echo(s: string) -> string`, then string-greps the per-target wrapper for `weaveffi_free_string` (or its named equivalent — `_weaveffiFreeString`, `_lib.weaveffi_free_string`, `weaveffi_free_string`, `NativeMethods.weaveffi_free_string`, `wasm.weaveffi_free_string`, `C.weaveffi_free_string`) and asserts at least one occurrence per target file. Run `cargo test --workspace` to verify nothing is broken.

### Phase 5 — Memory contract: bytes return free discipline

Same audit for `weaveffi_free_bytes`. The contract is that callers free with
`weaveffi_free_bytes(ptr, len)`.

- [ ] Audit every generator for `weaveffi_free_bytes` usage on `Bytes` returns. In each `crates/weaveffi-gen-*/src/lib.rs`, ensure the generated wrapper for any function returning `Bytes` calls the free helper after copying out the data. Add a test in each generator `{generator}_bytes_return_calls_free_bytes`. Add a cross-generator test `crates/weaveffi-cli/tests/bytes_return_free_audit.rs` mirroring the string test. Run `cargo test --workspace` to verify nothing is broken.

### Phase 6 — Memory contract: struct destructor discipline

Every generated struct wrapper must call its corresponding C `_destroy`
function exactly once. Audit and add tests.

- [ ] Audit every generator for struct destructor calls. In each `crates/weaveffi-gen-*/src/lib.rs`, ensure that for every IR struct, the wrapper class implements deterministic cleanup that calls `weaveffi_{module}_{Struct}_destroy`. C++: destructor and move-assignment null-out source. Swift: `deinit` calls destroy. Kotlin: `Closeable.close()` and JNI free. Node: handle wrapper has a finalizer or explicit `dispose()` that calls destroy via N-API. Python: `__del__` and `__exit__` of a context manager call destroy. .NET: `IDisposable.Dispose()` and `~Finalizer()` both call destroy idempotently. Dart: `dispose()` method, plus `Finalizer<Pointer>` registration. Go: `Close()` method plus optional `runtime.SetFinalizer`. Ruby: `FFI::AutoPointer` with custom release callback. Add a test in each generator `{generator}_struct_wrapper_calls_destroy`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a cross-generator test `crates/weaveffi-cli/tests/struct_destroy_audit.rs` named `every_generator_destroys_structs` that builds a one-struct API `Contact { id: i64, name: string }` and verifies each generated per-target file contains the matching destroy call name (`weaveffi_contacts_Contact_destroy`). Run `cargo test --workspace` to verify nothing is broken.

### Phase 7 — Memory contract: allocator alignment for .NET

The .NET generator's `WeaveFFIHelpers.StringToPtr` uses
`Marshal.StringToCoTaskMemUTF8` and `WeaveFFIHelpers.FreePtr` uses
`Marshal.FreeCoTaskMem`. The Rust runtime side allocates via the system
allocator, so passing a CoTaskMem-allocated string to Rust and expecting Rust
to free it (or vice versa) corrupts the heap on any platform where the two
allocators differ. Fix by routing all cross-boundary allocations through the
WeaveFFI ABI runtime.

- [ ] Add `weaveffi_alloc(size_t)` and `weaveffi_free(void*, size_t)` C ABI functions to `crates/weaveffi-abi/src/lib.rs`. They must use `std::alloc::alloc` / `std::alloc::dealloc` with a fixed `Layout` (alignment 1 for byte arrays). Export them as `#[no_mangle] pub extern "C"`. Add a test `alloc_and_free_round_trip` that allocates 64 bytes, writes a pattern, reads it back, frees. Add the prototypes to the C header generator output in `crates/weaveffi-gen-c/src/lib.rs` (next to `weaveffi_free_string`). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the .NET generator to use `weaveffi_alloc` / `weaveffi_free` via P/Invoke instead of `Marshal.StringToCoTaskMemUTF8` / `FreeCoTaskMem`. In `crates/weaveffi-gen-dotnet/src/lib.rs`, replace `WeaveFFIHelpers.StringToPtr` body with: P/Invoke `weaveffi_alloc(bytes.Length + 1)`, copy UTF-8 bytes via `Marshal.Copy`, write a NUL terminator. Replace `WeaveFFIHelpers.FreePtr` body with `weaveffi_free(ptr, len)` — note this requires tracking the original length, so change the helper signature to take `(IntPtr ptr, ulong len)`. The wrapper that allocated must remember the length and pass it back. Alternatively, use the allocation pattern only for outputs returned to native, since strings going IN can be NUL-terminated and freed via the byte-pinning approach from Phase 1. Update generated wrappers to use the chosen approach consistently. Add tests `dotnet_uses_weaveffi_alloc` and `dotnet_uses_weaveffi_free`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Document the allocator contract in `docs/src/guides/memory.md` (the file already exists; update it). Add a new section "## Allocator contract" explaining that all cross-boundary heap allocations must go through `weaveffi_alloc` / `weaveffi_free` (or the typed `weaveffi_free_string` / `weaveffi_free_bytes`); the system allocator on the consumer side must NEVER free Rust-allocated pointers and vice versa. Document `weaveffi_alloc` and `weaveffi_free` C signatures. Run `cargo test --workspace` to verify nothing is broken.

### Phase 8 — Validator hardening: reject features without codegen

`TypeRef::Callback` panics in 8+ generators with `todo!()`. We must either
implement it everywhere (Phase 9–13) or reject it at validation time. We
choose: implement it (it's a real feature), but in the meantime enforce that
the IR validator catches every feature-without-codegen combination so we
never reach a panic. Add a new "codegen capability matrix" mechanism.

- [ ] Add a `Capability` enum to `crates/weaveffi-core/src/codegen.rs` enumerating every IR feature that a generator may or may not support: `Callbacks`, `Listeners`, `Iterators`, `Builders`, `AsyncFunctions`, `CancellableAsync`, `TypedHandles`, `BorrowedTypes`, `MapTypes`, `NestedModules`, `CrossModuleTypes`, `ErrorDomains`, `DeprecatedAnnotations`. Add a `fn capabilities(&self) -> &'static [Capability]` method to the `Generator` trait with a default implementation returning all capabilities (i.e., generators are assumed feature-complete unless they say otherwise). Each generator overrides this to declare its actual support. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add per-target capability declarations in every generator. Update each `crates/weaveffi-gen-*/src/lib.rs` to override `capabilities()` returning only the features it currently fully supports. (After later phases finish each feature, the corresponding capability is added back.) Initial state per generator (current reality): C: all except Callbacks. C++: all except Callbacks, Listeners. Swift: all except Callbacks, Listeners. Android: all except Callbacks, Listeners. Node: all except Callbacks, Listeners, plus exclude CancellableAsync (broken). WASM: all except Callbacks, Listeners. Python: all except Callbacks, Listeners. .NET: all except Callbacks, Listeners. Dart: all except Callbacks, Listeners, Builders (build() throws). Go: all except Callbacks, Listeners, AsyncFunctions, Iterators, Builders. Ruby: all except Callbacks, Listeners, AsyncFunctions, Iterators. Add tests in each generator asserting the capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `validate_capabilities` pass to `crates/weaveffi-core/src/validate.rs` that, given an `Api` and a slice of generator capability sets, checks: for each IR feature actually used in the API, every selected generator must declare support. If not, return a new `ValidationError::TargetMissingCapability { target: String, capability: String, location: String }` variant. Add it to `ValidationError`. Add a corresponding `validation_suggestion` case in `crates/weaveffi-cli/src/main.rs`. Update `cmd_generate` to call `validate_capabilities` after `validate_api` (passing only the user-selected targets). Add tests in `crates/weaveffi-core/src/validate.rs`: `callback_in_api_with_node_target_rejected`, `iterator_in_api_with_go_target_rejected`, `async_in_api_with_ruby_target_rejected`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Replace every `todo!("callback ...")` / `todo!("...")` panic in generator codepaths with `unreachable!("validator should have rejected ...")` so that if the validator is bypassed the panic message is loud and clear. Audit all `crates/weaveffi-gen-*/src/lib.rs` and `crates/weaveffi-core/src/templates.rs`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 9 — Callback codegen end-to-end: IR and C

`TypeRef::Callback` is currently a panic site in every generator. Implement
it end-to-end starting with the C ABI. A callback in IR is a function pointer
that the user can pass to a function or store in a struct. The C ABI
representation is: `void (*name)(void* context, T1 arg1, T2 arg2, ...)`.

- [ ] Strengthen the callback IR shape in `crates/weaveffi-ir/src/ir.rs`. The `CallbackDef` struct should already exist (Phase 29 of PRD-v3) — verify it has `name: String`, `params: Vec<Param>`, `returns: Option<TypeRef>`, `doc: Option<String>`. Ensure the `Module` has `pub callbacks: Vec<CallbackDef>` with `#[serde(default)]`. Add a parser hook so `parse_type_ref("callback<MyCallback>")` returns `TypeRef::Callback("MyCallback".into())` referring to a callback by name. Update `type_ref_to_string` to format `Callback("MyCallback")` as `callback<MyCallback>`. Update Serialize/Deserialize impls. Change `TypeRef::Callback` to wrap a `String` (the callback name) instead of a `Box<CallbackDef>` if it currently does — the current PRD-v3 Phase 18 changed it to `Box<CallbackDef>` which conflates anonymous inline callbacks with named ones. Use named callbacks throughout. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add validator support for `TypeRef::Callback("Name")` in `crates/weaveffi-core/src/validate.rs`. In `validate_type_ref`, add an arm that checks the name refers to a `CallbackDef` defined in the same module (or in another module, qualified as `mod.Name` per cross-module rules from PRD-v3 Phase 27). If not, return `ValidationError::UnknownTypeRef { name }`. Also recursively validate the callback's `params` and `returns` use only valid types. Add tests `callback_ref_valid_passes`, `callback_ref_unknown_rejected`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the C generator in `crates/weaveffi-gen-c/src/lib.rs`. For each `CallbackDef` in each module, emit a typedef: `typedef {ReturnType} (*weaveffi_{module}_{CallbackName})(void* context{, {ParamType} {param_name}}*);`. Place callback typedefs after enum typedefs and before struct typedefs. For function parameters of type `Callback("Name")`, expand to `weaveffi_{module}_{Name} {param_name}, void* {param_name}_context` (the callback function pointer + a context pointer). For return types of type `Callback("Name")`, do the same expansion using the return position (out parameter). For struct fields of type `Callback("Name")`, generate getters that return the callback function pointer and setters that take it. Add tests `c_emits_callback_typedef`, `c_function_param_callback_uses_pointer_and_context`. Remove the C generator's `todo!("callback C type")` panic. Add `Callbacks` to the C generator's capability set. Run `cargo test --workspace` to verify nothing is broken.

### Phase 10 — Callback codegen: Swift, Kotlin, C++

- [ ] Implement callback emission in the Swift generator. In `crates/weaveffi-gen-swift/src/lib.rs`, for each `CallbackDef` emit a Swift typealias: `public typealias {CallbackName} = ({ParamTypes}) -> {ReturnType}`. For function parameters of type `Callback("Name")`, generate a Swift wrapper that takes a Swift closure, wraps it via `Unmanaged<AnyObject>.passRetained` or similar bridging machinery, and passes the C function pointer + context pointer. The C function pointer must be a `@convention(c)` static trampoline that retrieves the closure from the context and invokes it. Add a test `swift_emits_callback_typealias_and_wraps_closure`. Add `Callbacks` to the Swift capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the Android/Kotlin generator. In `crates/weaveffi-gen-android/src/lib.rs`, for each `CallbackDef` emit a Kotlin function type alias: `typealias {CallbackName} = ({ParamTypes}) -> {ReturnType}`. For JNI-side codegen, emit a global JNI registry indexed by a `jlong` token, plus a static C function that fetches the kotlin lambda from the registry and invokes it via `CallVoidMethod` / `CallObjectMethod`. The user-facing Kotlin wrapper takes a Kotlin lambda, registers it, and passes (callback_pointer, context = registry_id) to the C function. Add a test `kotlin_emits_callback_typealias_and_jni_registry`. Add `Callbacks` to the Android capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the C++ generator. In `crates/weaveffi-gen-cpp/src/lib.rs`, for each `CallbackDef` emit a C++ typedef using `std::function`: `using {CallbackName} = std::function<{ReturnType}({ParamTypes})>;`. For function parameters, the wrapper allocates a `std::function` on the heap, passes a static C trampoline that calls it via the context pointer, and registers the heap-owned function for later cleanup. Add a test `cpp_emits_callback_typedef_using_std_function`. Add `Callbacks` to the C++ capability set. Run `cargo test --workspace` to verify nothing is broken.

### Phase 11 — Callback codegen: Node, Python, .NET

- [ ] Implement callback emission in the Node generator. In `crates/weaveffi-gen-node/src/lib.rs`, the TypeScript declaration emits a callback type as a TS function type: `export type {CallbackName} = ({params}) => {returnType}`. The N-API wrapper for a function with a callback parameter creates a `napi_threadsafe_function` from the user-supplied JS callback, passes its pointer + context to the C function, and frees it after the call (or stores it for listener-style callbacks). Update `weaveffi_addon.c` generation accordingly. Add a test `node_emits_callback_type_and_threadsafe_function`. Add `Callbacks` to the Node capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the Python generator. In `crates/weaveffi-gen-python/src/lib.rs`, for each `CallbackDef` emit a `ctypes.CFUNCTYPE` definition: `_{CallbackName} = ctypes.CFUNCTYPE({ReturnCType}, ctypes.c_void_p, *{ParamCTypes})`. For function parameters of type `Callback("Name")`, the Python wrapper accepts a Python callable, wraps it via the CFUNCTYPE constructor (which keeps the trampoline alive), and passes (cfunc, ctypes.c_void_p(0)) — the trampoline ignores the context and calls the wrapped Python function directly. Hold a reference to the cfunc on the wrapper to prevent GC. Update `weaveffi.pyi` to declare callback types as Python callables (`Callable[..., ...]`). Add a test `python_emits_callback_cfunctype`. Add `Callbacks` to the Python capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the .NET generator. In `crates/weaveffi-gen-dotnet/src/lib.rs`, for each `CallbackDef` emit a C# delegate type: `public delegate {ReturnCSType} {CallbackName}({ParamCSTypes});` with `[UnmanagedFunctionPointer(CallingConvention.Cdecl)]`. For function parameters of type `Callback("Name")`, the wrapper accepts the delegate, pins it via `GCHandle.Alloc(d, GCHandleType.Normal)`, gets the function pointer via `Marshal.GetFunctionPointerForDelegate`, passes (ptr, IntPtr.Zero) to the P/Invoke, and stores the GCHandle on a static collection until the callback is no longer needed. Add a test `dotnet_emits_callback_delegate_and_pins_via_gc_handle`. Add `Callbacks` to the .NET capability set. Run `cargo test --workspace` to verify nothing is broken.

### Phase 12 — Callback codegen: Dart, Go, Ruby, WASM

- [ ] Implement callback emission in the Dart generator. In `crates/weaveffi-gen-dart/src/lib.rs`, for each `CallbackDef` emit a Dart typedef: `typedef {CallbackName} = {ReturnType} Function({ParamTypes});`. The wrapper accepts a Dart callable, wraps it via `Pointer.fromFunction` (or `NativeCallable.isolateLocal` for newer Dart SDKs), and passes (pointer, nullptr context). Add a test `dart_emits_callback_typedef_using_pointer_from_function`. Add `Callbacks` to the Dart capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the Go generator. In `crates/weaveffi-gen-go/src/lib.rs`, Go cgo cannot directly take a Go function pointer as a C callback. Use the standard pattern: emit an exported C wrapper function (via `//export Name`) that calls back into the Go runtime via a registry. For each `CallbackDef`, emit a Go function type: `type {CallbackName} func({ParamTypes}) {ReturnType}`. The wrapper registers the user callback in a `sync.Map` keyed by an int64 token, passes the static C trampoline pointer + token as context to the C function. Add a test `go_emits_callback_func_type_and_registry`. Add `Callbacks` to the Go capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the Ruby generator. In `crates/weaveffi-gen-ruby/src/lib.rs`, use Ruby FFI's `callback` macro: emit `callback :{callback_name}, [{ParamFFITypes}], {ReturnFFIType}`. The wrapper accepts a Ruby Proc, passes it directly to the FFI-attached function (the FFI gem manages the trampoline and lifetime). Add a test `ruby_emits_callback_via_ffi_callback`. Add `Callbacks` to the Ruby capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement callback emission in the WASM generator. In `crates/weaveffi-gen-wasm/src/lib.rs`, WASM callbacks require a function table. Emit a JS helper `_registerCallback(wasm, jsFn)` that appends `jsFn` to `wasm.__indirect_function_table` (via `Table.grow`) and returns the new index. The wrapper passes (table_index, context) to the WASM export. Update the `.d.ts` to declare callback types as JS function types. Add a test `wasm_emits_callback_registration_helper_and_table_index`. Add `Callbacks` to the WASM capability set. Run `cargo test --workspace` to verify nothing is broken.

### Phase 13 — Callback parity test and template engine update

- [ ] Add a cross-generator callback parity test in `crates/weaveffi-cli/tests/cli_callbacks.rs`. Build an API with a callback `OnTick(value: i32)` and a function `register_ticker(callback: callback<OnTick>) -> i32`. Generate for all 11 targets and assert each output file contains the callback typedef/typealias/delegate/Proc-equivalent. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update `crates/weaveffi-core/src/templates.rs` `type_ref_to_map` to emit a sane representation for `TypeRef::Callback(name)` instead of `todo!()`. Return a `HashMap` with `kind = "callback"` and `name = "{the_name}"`. Add a test `template_context_callback_no_panic`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the scaffold generator in `crates/weaveffi-cli/src/scaffold.rs` to emit Rust callback parameter types. For a function param of type `Callback("Name")`, emit `extern "C" fn(*mut std::ffi::c_void{, {RustParamType}}*) {{-> {RustReturnType}}}? as the type, plus a `*mut std::ffi::c_void` context parameter. Add a test `scaffold_callback_param_emits_function_pointer`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 14 — Listeners codegen end-to-end

The C generator already emits `register_*` and `unregister_*` for listeners,
but Swift, C++, Android, Node, WASM, Python, .NET, Dart, Go, and Ruby ignore
the IR's `listeners` field entirely. Implement listener wrappers everywhere.

- [ ] Add listener wrapper emission to the Swift generator. For each `ListenerDef` in a module, emit a Swift class `{ListenerName}` with a `register(_ callback: {EventCallbackType}) -> UInt64` method that calls `weaveffi_{module}_register_{listener}` and returns the listener ID, plus a `unregister(_ id: UInt64)` static method. The class internally pins the Swift closure via `Unmanaged` and unpins on unregister. Add a test `swift_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the Android generator. Emit a Kotlin class `{ListenerName}` with `register(callback: {EventCallbackType}): Long` and `unregister(id: Long)`. JNI bridge follows the same registry pattern as callbacks. Add a test `kotlin_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the C++ generator. Emit a `class {ListenerName}` with `static uint64_t register(std::function<...>)` and `static void unregister(uint64_t)`. Use a heap-allocated `std::function` registry. Add a test `cpp_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the Node generator. Emit a JS class with `register(callback: Function): bigint` calling the threadsafe-function-backed N-API binding, and `unregister(id: bigint)`. Update TypeScript declarations. Add a test `node_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the Python generator. Emit a Python class with `@staticmethod def register(callback: Callable[...,...]) -> int` and `@staticmethod def unregister(id: int)`. Hold the cfunc reference in a class-level dict to prevent GC. Add a test `python_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the .NET generator. Emit a static `class {ListenerName}` with `public static ulong Register({DelegateType} callback)` and `public static void Unregister(ulong id)`. Maintain a `Dictionary<ulong, GCHandle>` for pinning. Add a test `dotnet_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the Dart generator. Emit a Dart class `{ListenerName}` with `static int register({CallbackType} callback)` and `static void unregister(int id)`. Use `Pointer.fromFunction` and a `Map<int, ...>` for pinning. Add a test `dart_emits_listener_class`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the Go generator. Emit a Go type `type {ListenerName} struct{}` with methods `Register(cb {CallbackType}) uint64` and `Unregister(id uint64)`. Use a `sync.Map` for pinning. Add a test `go_emits_listener_type`. Add `Listeners` to the Go capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the Ruby generator. Emit a `module {ListenerName}` with `self.register(&block)` and `self.unregister(id)` methods. Use an `@@callbacks = {}` class variable for pinning. Add a test `ruby_emits_listener_module`. Add `Listeners` to the Ruby capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add listener wrapper emission to the WASM generator. Emit a JS object exposing `register` and `unregister` per listener, using the function table indirection from the callback support phase. Add a test `wasm_emits_listener_object`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `Listeners` to every generator's capability set now that they all support it. Add a cross-generator parity test `crates/weaveffi-cli/tests/cli_listeners.rs` named `listener_register_unregister_emitted_for_all_targets` that uses the existing `samples/events/events.yml` and asserts each target output contains the listener registration helper. Run `cargo test --workspace` to verify nothing is broken.

### Phase 15 — Iterator codegen end-to-end

The C/Swift/Python iterator handling works. The Node iterator return is
broken (calls `_next` with the wrong signature per PRD audit). The Ruby
iterator is grouped with `List` (materialises into an Array). The Go
iterator path is not implemented. Fix all three.

- [ ] Fix the Node iterator return ABI in `crates/weaveffi-gen-node/src/lib.rs`. For a function returning `Iterator(T)`, the C ABI shape is: returns an opaque iterator pointer, then the consumer calls `iter_next(iter, *out_item, *out_err) -> int32_t` which returns 1 (item available), 0 (done), or -1 (error). The current Node code calls `_next(result, &iter_item)` which is wrong. The wrapper should: get the iterator pointer, then in a loop call `_next(iter, &out_item, &err)` correctly, accumulating into a JS array — or, for a true streaming experience, return a JS async iterator (`Symbol.asyncIterator`). Implement the streaming variant: return an object with `[Symbol.asyncIterator]() { return { async next() { ... call _next ... } } }`. Add a test `node_iterator_return_uses_correct_next_signature`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement true iterator streaming in the Ruby generator. In `crates/weaveffi-gen-ruby/src/lib.rs`, for a function returning `Iterator(T)` emit a Ruby method that returns an `Enumerator.new { |y| loop { ... call iter_next ... y << item } }`. Do not materialise into an Array. Add a test `ruby_iterator_return_uses_lazy_enumerator`. Add `Iterators` to the Ruby capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement iterator support in the Go generator. In `crates/weaveffi-gen-go/src/lib.rs`, for a function returning `Iterator(T)` emit a Go function returning `<-chan T` (read-only channel). The implementation spawns a goroutine that calls `_next` in a loop and writes items to the channel, closing on done. Add a test `go_iterator_return_uses_channel`. Add `Iterators` to the Go capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a cross-generator iterator parity test `crates/weaveffi-cli/tests/cli_iterators.rs` named `iterator_return_emits_streaming_in_all_targets` using the existing `samples/events/events.yml` (which has `get_messages() -> iter<string>`). Assert each target contains the appropriate streaming construct (Swift Sequence, Kotlin Iterator, Python __next__, .NET IEnumerable, Dart Stream, Node async iterator, Ruby Enumerator, Go channel, C++ iterator class, C iter_next + iter_destroy, WASM async iterator). Run `cargo test --workspace` to verify nothing is broken.

### Phase 16 — Builder pattern: Dart `build()` and Go cross-target completion

- [ ] Fix the Dart builder to actually call the C `_Builder_build` function instead of throwing `UnimplementedError`. In `crates/weaveffi-gen-dart/src/lib.rs`, for each builder struct emit FFI typedefs and lookups for `weaveffi_{module}_{Struct}Builder_new`, `_set_{field}` per field, `_build`, `_destroy`. The Dart builder class must hold a `Pointer<Void> _handle = _builderNew()` in the constructor, accumulate setter calls into native setters, and `build()` calls `_builderBuild(_handle, &err)` returning a wrapped struct, then `_builderDestroy(_handle)` (reset handle to null). Add a test `dart_builder_build_calls_native`. Add `Builders` to the Dart capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Complete the Go builder. In `crates/weaveffi-gen-go/src/lib.rs`, replace the current map-based `WithFoo` placeholders with a typed builder that calls C: `func New{Struct}Builder() *{Struct}Builder { return &{Struct}Builder{handle: C.weaveffi_..._Builder_new()} }`, with `func (b *{Struct}Builder) WithField(v T) *{Struct}Builder { C.weaveffi_..._Builder_set_field(b.handle, v); return b }` and `func (b *{Struct}Builder) Build() (*{Struct}, error) { ... }`. Add a `Close()` for cleanup. Add a test `go_builder_build_calls_c_builder_build`. Add `Builders` to the Go capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a cross-generator builder parity test `crates/weaveffi-cli/tests/cli_builders.rs` named `builder_pattern_emits_real_build_in_all_targets`. Build a one-struct API with `builder: true` and 3 fields; for each target string-grep the wrapper for the builder construct and verify the `build()` (or equivalent) calls the C `_Builder_build` symbol — not a placeholder. Run `cargo test --workspace` to verify nothing is broken.

### Phase 17 — Async support for Go and Ruby

- [ ] Implement Go async function support in `crates/weaveffi-gen-go/src/lib.rs`. Currently async functions are silently filtered out. For each async function in the IR, emit a Go function that returns `<-chan {Result}` where `Result` is a struct `{Value T, Err error}`. The implementation spawns a goroutine that calls the `_async` C entry point with a registered Go callback (via the same registry pattern as callbacks), the callback writes the result to the channel, the goroutine closes it. Remove the async-skip filter and the `async_functions_skipped` test (or convert it to assert async IS handled). Add a test `go_async_returns_channel`. Add `AsyncFunctions` to the Go capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Implement Ruby async function support in `crates/weaveffi-gen-ruby/src/lib.rs`. Currently async functions are silently filtered out. For each async function emit a Ruby method `def self.{name}_async(args, &block); ...; end` that takes a block, registers it in the callback registry, calls `weaveffi_{module}_{name}_async(args, registry_id, ...)`, and yields the result when the block is invoked. For Ruby Fiber/async-gem users, also emit `def self.{name}(args)` returning a `Concurrent::Promise` (require the `concurrent-ruby` gem in the gemspec). Remove the async-skip filter. Add a test `ruby_async_emits_block_and_promise_versions`. Add `AsyncFunctions` to the Ruby capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a cross-generator async parity test `crates/weaveffi-cli/tests/cli_async_parity.rs` named `async_function_emitted_in_all_targets` using the existing `samples/async-demo/async_demo.yml`. Assert each target output contains the appropriate async construct (Swift `async throws`, Kotlin `suspend fun`, Python `async def`, .NET `async Task`, Dart `Future`, Node `Promise`, Ruby `Promise`/block, Go channel, C++ `std::future`, C `_async` + callback, WASM `Promise`). Run `cargo test --workspace` to verify nothing is broken.

### Phase 18 — Cancellation token wiring across all generators

PRD-v3 Phase 22 added `cancellable: bool` to `Function` and a `weaveffi_cancel_token`
runtime, but the Node generator passes `NULL` for the token in async functions
marked cancellable, and other generators were never audited.

- [ ] Audit and fix Node cancellable async in `crates/weaveffi-gen-node/src/lib.rs`. Currently `if (f.cancellable) { c_args.push("NULL".into()); }` — replace with: extract a JS `AbortSignal` from the call site, create a native `weaveffi_cancel_token` via the ABI, register an `abort` listener that calls `weaveffi_cancel_token_cancel`, pass the token to the C function, and `weaveffi_cancel_token_destroy` on completion. Update the TypeScript declaration to add a final `signal?: AbortSignal` parameter. Add a test `node_cancellable_async_passes_real_token`. Add `CancellableAsync` to the Node capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Audit cancellable wiring in Swift generator. Update `crates/weaveffi-gen-swift/src/lib.rs` for cancellable async functions: the Swift `async throws` wrapper must accept an optional `Task` cancellation context. Use `withTaskCancellationHandler` to wire `Task.isCancelled` to `weaveffi_cancel_token_cancel`. Add a test `swift_cancellable_async_uses_task_cancellation_handler`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Audit cancellable wiring in Kotlin generator. Update `crates/weaveffi-gen-android/src/lib.rs` for cancellable async functions: the Kotlin `suspend fun` must use `suspendCancellableCoroutine` with `invokeOnCancellation { weaveffiCancelTokenCancel(token) }`. Add a test `kotlin_cancellable_async_uses_invoke_on_cancellation`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Audit cancellable wiring in Python, .NET, Dart, Go, Ruby, C++, WASM. For each, update the async wrapper for cancellable functions to wire the platform's cancellation primitive (Python `asyncio.CancelledError`, .NET `CancellationToken`, Dart `Future.timeout` or custom `CancelToken`, Go `context.Context`, Ruby `Concurrent::Cancellation`, C++ `std::stop_token`, WASM `AbortSignal`) to `weaveffi_cancel_token_cancel`. Add a test per generator. Add `CancellableAsync` to each capability set. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a cross-generator cancellation parity test in `crates/weaveffi-cli/tests/cli_cancellation_parity.rs`. Build an API with `cancellable: true` async functions for all targets and assert each output contains the platform-appropriate cancellation primitive. Run `cargo test --workspace` to verify nothing is broken.

### Phase 19 — Native library naming via `c_prefix` everywhere

The `c_prefix` config option currently only affects the C generator (header
filename and symbol prefix). Every other generator hardcodes `weaveffi.h`,
`libweaveffi.dylib`, `weaveffi.dll`, `-lweaveffi`, etc. Custom prefixes break
all non-C generators. Fix by threading `c_prefix` through everywhere.

- [ ] Update the Swift generator to respect `c_prefix`. In `crates/weaveffi-gen-swift/src/lib.rs`, override `generate_with_config` to read `config.c_prefix()`. The generated `module.modulemap` must reference `header "../../c/{c_prefix}.h"` and `link "{c_prefix}"`. The C system module directory should be named `C{PascalCasePrefix}` (default `CWeaveFFI`). The `import C{PascalCasePrefix}` in the Swift source must match. Add a test `swift_modulemap_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Node generator to respect `c_prefix`. In `crates/weaveffi-gen-node/src/lib.rs`, the `binding.gyp` `libraries` field must use `-l{c_prefix}` (Linux/macOS) and the `include_dirs` must point at the C output directory. The `weaveffi_addon.c` `#include` must reference `{c_prefix}.h`. Add a test `node_binding_gyp_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Python generator to respect `c_prefix`. In `crates/weaveffi-gen-python/src/lib.rs`, `_load_library` must use `lib{c_prefix}.dylib` / `.so` / `{c_prefix}.dll`. The generated function bodies that call `_lib.weaveffi_X_Y` must call `_lib.{c_prefix}_X_Y`. Add a test `python_load_library_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the .NET generator to respect `c_prefix`. In `crates/weaveffi-gen-dotnet/src/lib.rs`, the `LibName` constant must be `{c_prefix}` and every `[DllImport(LibName, EntryPoint = "weaveffi_X_Y")]` attribute must use `EntryPoint = "{c_prefix}_X_Y"`. Add a test `dotnet_dllimport_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Dart generator to respect `c_prefix`. In `crates/weaveffi-gen-dart/src/lib.rs`, `_openLibrary` must use the prefix in the platform-specific library names, and `lookupFunction` calls must reference `{c_prefix}_X_Y`. Add a test `dart_open_library_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Go generator to respect `c_prefix`. In `crates/weaveffi-gen-go/src/lib.rs`, the cgo preamble must `#include "{c_prefix}.h"` and `LDFLAGS: -l{c_prefix}`, and every `C.weaveffi_X_Y` must become `C.{c_prefix}_X_Y`. Add a test `go_cgo_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Ruby generator to respect `c_prefix`. In `crates/weaveffi-gen-ruby/src/lib.rs`, `ffi_lib '{c_prefix}'` must use the prefix, and every `attach_function :weaveffi_X_Y` must become `attach_function :{c_prefix}_X_Y`. Add a test `ruby_ffi_lib_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the WASM generator to respect `c_prefix`. In `crates/weaveffi-gen-wasm/src/lib.rs`, every `wasm.weaveffi_X_Y` call in the generated JS must become `wasm.{c_prefix}_X_Y`, and the README documentation must reference `{c_prefix}_alloc`/`{c_prefix}_error_clear` etc. Add a test `wasm_js_calls_respect_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the C++ generator to respect `c_prefix`. In `crates/weaveffi-gen-cpp/src/lib.rs`, the `extern "C"` block must declare `{c_prefix}_X_Y` functions, the `CMakeLists.txt` must `target_link_libraries(... INTERFACE {c_prefix})`, and the README must reflect the prefix. Add a test `cpp_links_respect_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the Android generator to respect `c_prefix`. In `crates/weaveffi-gen-android/src/lib.rs`, `System.loadLibrary("{c_prefix}")` in Kotlin and the JNI bridge `#include "{c_prefix}.h"` must use the prefix. The CMakeLists.txt must reference the prefix-named library. Add a test `android_loads_native_lib_respects_c_prefix`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a cross-generator c_prefix parity test in `crates/weaveffi-cli/tests/cli_c_prefix_parity.rs`. Generate from a TOML config setting `c_prefix = "mylib"` and assert every target's wrapper file references `mylib` (not `weaveffi`) for the library name and symbol prefix. Run `cargo test --workspace` to verify nothing is broken.

### Phase 20 — `output_files_with_config` for every generator

Every generator with a configurable package/namespace name must override
`output_files_with_config` so `weaveffi generate --dry-run` reports accurate
paths.

- [ ] Audit and implement `output_files_with_config` in every generator that has any config-driven output path. Specifically: Swift (`swift_module_name` affects `Sources/{name}/...`), Android (`android_package` affects `src/main/kotlin/{package_path}/...`), Node (`node_package_name` is in `package.json` only — no path effect, but verify), Python (`python_package_name` affects `python/{name}/...`), .NET (`dotnet_namespace` is content-only — verify), Dart (`dart_package_name` is in `pubspec.yaml`), Go (`go_module_path` is in `go.mod`), Ruby (`ruby_gem_name` may affect `ruby/lib/{gem_name}.rb` if implemented; `ruby_module_name` is content-only), C++ (`cpp_header_name` affects the header filename), WASM (`wasm_module_name` affects `{name}.js` and `{name}.d.ts`), C (`c_prefix` affects `{prefix}.h` and `{prefix}.c`). For each affected generator, override `output_files_with_config` to return the correct paths. Add a test in each generator named `{generator}_output_files_with_config_respects_naming`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a CLI `--dry-run` test verifying that when the user customises every config option, the printed file paths exactly match the actual emitted files. In `crates/weaveffi-cli/tests/cli_dry_run_paths.rs`, test `dry_run_paths_match_real_outputs_with_full_custom_config`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 21 — Inline `[generators]` section validation

Currently `merge_inline_generators` in `crates/weaveffi-cli/src/main.rs`
silently drops unknown keys in the IDL's `generators:` section, so typos
cause silent misconfiguration.

- [ ] Validate the inline `[generators]` section against `GeneratorConfig`'s known fields. In `crates/weaveffi-cli/src/main.rs`, replace the hand-written `merge_inline_generators` mapping with a typed deserialization: define a struct `InlineGeneratorsSection` matching the supported fields (per-target inline configs as nested structs), use `toml::Value::try_into::<InlineGeneratorsSection>()`. If the user provides an unknown key, `serde` will report it via `deny_unknown_fields`. Convert the TOML deserialization error into a `ValidationError::UnknownGeneratorConfigKey { key: String, target: String }` and surface it with a clear suggestion: "valid keys for the `{target}` generator section are: ...". Add tests `inline_generators_unknown_key_rejected`, `inline_generators_typed_deser_works`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 22 — Reproducibility: canonical IR hashing

`cache::hash_api` uses `serde_json::to_string(api)` which serialises HashMaps
in unspecified order and emits floats with platform-dependent formatting.
Two equivalent APIs may hash differently across runs.

- [ ] Replace the cache hash in `crates/weaveffi-core/src/cache.rs` with a canonical serialisation. Implement `fn canonical_serialize(api: &Api) -> String` that walks the `Api` and emits a deterministic byte representation: sort all `HashMap`/`BTreeMap` keys lexicographically, format floats with a fixed precision (`format!("{:.17}", f)`), use a fixed key order for every struct (alphabetical), and write everything to a single byte buffer (e.g., a CBOR-like layout, or a sorted JSON via `serde_json::Map<String, Value>` with `BTreeMap::extend` of sorted entries). Hash the canonical bytes with SHA-256. Add a property test `hash_invariant_under_hashmap_iteration_order` that builds an `Api` with a `HashMap` populated in two different orders and asserts the hash is identical. Add `hash_stable_across_serde_versions` (a known-fixture test that the hex digest of a specific API matches a hardcoded expected value). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Make cache writes atomic in `crates/weaveffi-core/src/cache.rs`. `write_cache` should write to a temp file `.weaveffi-cache.tmp.{pid}.{nanos}` then `rename` to `.weaveffi-cache`. Use `std::fs::rename` (POSIX atomic) or on Windows the equivalent. Add a test `cache_write_is_atomic_under_concurrent_writers` that spawns N threads each writing the cache and asserts the final file is non-corrupt. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Make the cache skip message suppressible in `crates/weaveffi-core/src/codegen.rs`. The "No changes detected, skipping code generation." message currently goes to stdout regardless of CLI verbosity. Add a `quiet: bool` field to `Orchestrator` (or change `print` to a `tracing::info!` call). Update the CLI to honour `--quiet`. Add a test `quiet_flag_suppresses_cache_skip_message`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 23 — IR schema version stamping in generated outputs

Generated files have no record of which IR version produced them. Stamp a
header in every output so consumers can detect drift.

- [ ] Add a `stamp_header(generator_name: &str) -> String` helper to `crates/weaveffi-core/src/codegen.rs`. It returns a comment like `"WeaveFFI {ir_version} {generator_name} {tool_version} - DO NOT EDIT - regenerate with 'weaveffi generate'"`. Use the workspace `CARGO_PKG_VERSION` for `tool_version`. Update each generator to prepend this header (with the appropriate comment syntax: `//`, `#`, `/* */`) to every generated source file. Add a test in each generator `{generator}_outputs_have_version_stamp`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `weaveffi check-stamp` CLI subcommand in `crates/weaveffi-cli/src/main.rs`. `CheckStamp { dir: String, expected_ir_version: Option<String> }` walks the directory, parses the stamp from each file, and reports any file whose IR version doesn't match the `Api` version specified by the user (or any file missing a stamp). Exit 0 if all stamps match, exit 1 otherwise. Add a test `check_stamp_passes_for_freshly_generated_dir`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 24 — Lockfile: `weaveffi.lock`

For reproducible builds, write a lockfile recording the IR hash, the
WeaveFFI tool version, and per-generator output file hashes. Consumers can
commit this file and CI can verify regeneration is deterministic.

- [ ] Add lockfile generation to the Orchestrator. In `crates/weaveffi-core/src/codegen.rs`, after generating, walk the output directory and compute SHA-256 of every emitted file. Write a TOML file `weaveffi.lock` to the output dir root with: `[meta] ir_version, tool_version, generated_at`. `[hash] api = "<sha256>"`. `[files."c/weaveffi.h"] = "<sha256>"`. (etc. for every generated file). Add a CLI flag `--lockfile / --no-lockfile` (default on). Add a test `lockfile_written_and_round_trips`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `weaveffi verify` CLI subcommand. `Verify { dir: String, lockfile: Option<String> }` re-hashes every file in `dir` and compares to the recorded hashes in `weaveffi.lock`, exit 0 if all match, exit 1 with a list of diffs otherwise. Add a test `verify_succeeds_for_unchanged_dir` and `verify_fails_when_file_modified`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 25 — Diff and Doctor exit codes

Both subcommands always exit 0 today, defeating CI gating.

- [ ] Make `weaveffi diff` exit non-zero when differences are found. In `crates/weaveffi-cli/src/main.rs` `cmd_diff`, track whether any file printed a diff (or `[new file]` / `[would be removed]`) and return `Err(...)` (translated to exit 1 by `color-eyre`) when so. Add a `--exit-code` flag (default true) so users who want the previous behaviour can set `--no-exit-code`. Add a test `diff_exits_nonzero_when_changes`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Make `weaveffi doctor` exit non-zero when a required tool is missing. Categorise each check as "required" (rustc, cargo, weaveffi-cli itself) or "optional" (xcodebuild, ndk-build, node, npm, wasm-pack, wasm-bindgen, target-installed). If any required check fails, exit 1. If all required pass but some optional fail, exit 0 with warnings. Add a `--all` flag to require all (including optional) checks. Add a test `doctor_exits_nonzero_when_required_missing`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 26 — JSON output mode for CLI

For IDE / CI integration, every CLI subcommand should support `--format json`.

- [ ] Add a global `--format` flag to the CLI in `crates/weaveffi-cli/src/main.rs`. Accept values `text` (default) and `json`. Update each subcommand handler to respect the flag: `validate` emits `{"ok": true, "modules": N, "functions": N, "structs": N, "enums": N, "warnings": [...]}` or `{"ok": false, "errors": [{"location": ..., "message": ..., "suggestion": ...}]}`. `lint` emits an array of warnings. `diff` emits per-file `{"path": ..., "status": "added"|"changed"|"removed", "patch": "..."}`. `doctor` emits per-check `{"name": ..., "ok": true|false, "version": ..., "hint": ...}`. `dry-run` emits an array of files. `extract` already produces structured output but supports the global `--format json` for the resulting Api. Add tests for each subcommand in `crates/weaveffi-cli/tests/cli_json_output.rs`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 27 — CLI source-position errors

YAML parse errors surface line/column info, but the CLI prints a plain text
message. Add a Rust-compiler-style ASCII rendering with `^` underlines.

- [ ] Add a `pretty_parse_error` helper to `crates/weaveffi-cli/src/main.rs` that takes a parse error (with line + column from `serde_yaml::Error` or equivalent) and the original input file content, and renders a multi-line error: filename:line:col, the source line, and a `^^^` underline for the offending span. Use `color-eyre` for ANSI colour. Apply it in `cmd_validate`, `cmd_generate`, `cmd_extract`, `cmd_lint`, `cmd_diff`. Add a test `parse_error_shows_source_with_caret`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Improve validation error rendering. `ValidationError` variants currently include a `module` and sometimes a `function` field, but no precise source location since validation runs after parse. Augment the parser to record `Span { line: u32, col: u32 }` for each parsed item (struct, function, field, variant). Plumb the spans through the validator so that, e.g., `DuplicateFunctionName` carries `Span` data and can be rendered by `pretty_parse_error`. Add a test `validation_error_shows_source_position`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 28 — `weaveffi targets` and `weaveffi explain` subcommands

Inspectability subcommands for users who want to discover capabilities.

- [ ] Add `weaveffi targets` to `crates/weaveffi-cli/src/main.rs`. The subcommand prints a table of every available target, the language, the runtime requirement (e.g., "Rust >= 1.74", "Node >= 18", "Python >= 3.8"), the support status ("stable", "experimental"), and the file the generator emits. Add JSON output via the global `--format` flag. Add a test `targets_lists_all_eleven`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `weaveffi explain <error_code>` to `crates/weaveffi-cli/src/main.rs`. Each `ValidationError` variant gets a stable error code (e.g. `WFFI001`, `WFFI002`, ...). Maintain a static table mapping code → markdown explanation (similar to `rustc --explain`). Add the codes as a discriminant on the error variants. Update the CLI error printer to include the code in the rendered message. Add a test `explain_unknown_code_returns_helpful_message`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 29 — `weaveffi format` subcommand for canonical IDL

Some users want to commit canonicalised IDL files (sorted keys, consistent
indentation). Add a `format` subcommand.

- [ ] Add `weaveffi format <input>` to `crates/weaveffi-cli/src/main.rs`. Parse the input file, then re-serialise it in canonical form: `version` first, then sorted modules by name, within each module sorted: enums, structs, callbacks, listeners, errors, functions; within each struct sorted fields; etc. Use 2-space YAML indentation. Add `--check` flag that exits 0 if the file is already canonical, exit 1 if not (without writing). Add `--write` flag that overwrites the input file in place (default behaviour: print to stdout). Add a test `format_canonicalises_module_order` and `format_check_detects_non_canonical`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 30 — `weaveffi build` subcommand: one-shot generate + cargo build

For users who want a single command that goes from IDL to a working native
library + bindings, add `build`.

- [ ] Add `weaveffi build` to `crates/weaveffi-cli/src/main.rs`. `Build { input: String, out: Option<String>, target: Option<String>, cargo_target: Option<String>, profile: Option<String> }` runs: (1) `weaveffi generate` (with the supplied flags), (2) detects whether the current directory is a Cargo crate (look for `Cargo.toml`), (3) runs `cargo build --release` (or the chosen profile) for the host or specified `--cargo-target` (e.g., `aarch64-apple-ios`, `aarch64-linux-android`, `wasm32-unknown-unknown`), (4) reports the path to the emitted shared library / static library / wasm module. Honour `--quiet` and `--verbose` for cargo's output. Add a test `build_calls_cargo_build_and_reports_artifact_path` (use `tempfile::tempdir`, init a minimal cdylib crate, run `weaveffi build`, assert the `.dylib`/`.so`/`.dll` exists at the reported path). Run `cargo test --workspace` to verify nothing is broken.

### Phase 31 — `weaveffi check` subcommand: validate + lint shortcut

- [ ] Add `weaveffi check <input>` to `crates/weaveffi-cli/src/main.rs`. It runs `validate` then `lint` and reports both. Useful in pre-commit hooks. Add `--strict` flag that treats warnings as errors (exit 1 on any warning). Add a test `check_runs_both_validate_and_lint`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 32 — `weaveffi watch` subcommand: re-generate on IDL change

- [ ] Add `notify` (file watcher) as a workspace dependency: `notify = "6"`. Add it to `crates/weaveffi-cli/Cargo.toml`. In `crates/weaveffi-cli/src/main.rs` add a `Watch { input: String, out: Option<String>, target: Option<String>, debounce_ms: Option<u64> }` subcommand. The handler creates a recursive `Watcher` rooted at the input file's parent directory, watches for `Modify` and `Create` events on `input` (and any `.tera` files in the templates dir if `--templates` is set), debounces by the configured ms (default 200), and re-runs `cmd_generate` on each change. Print a single status line "Watching {input}... Press Ctrl+C to exit." Add a test `watch_regenerates_on_input_change` (tricky — use `notify::EventKind::Modify`, write to the file, assert regeneration). Run `cargo test --workspace` to verify nothing is broken.

### Phase 33 — `weaveffi init` subcommand: in-place project scaffolding

- [ ] Add `weaveffi init` to `crates/weaveffi-cli/src/main.rs`. Like `new` but operates in the current directory. `Init { name: Option<String>, force: bool }`. If `name` is omitted, derive from the current directory name. If files already exist (`weaveffi.yml`, `Cargo.toml`, `src/lib.rs`), refuse unless `--force`. Add a test `init_in_empty_dir_works` and `init_refuses_existing_files`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 34 — Plugin / external generator system

For extensibility, allow users to install third-party generators on
`$PATH` (e.g., `weaveffi-gen-erlang`) and have the CLI discover them.

- [ ] Add an external generator discovery mechanism in `crates/weaveffi-cli/src/main.rs`. On startup, scan `$PATH` for binaries named `weaveffi-gen-*`. For each discovered binary, record `name = "<suffix after weaveffi-gen->"` and `path`. When `--target` includes such a name, invoke the binary with `weaveffi-gen-X --api <api.json> --out <dir>` (the API is serialised to JSON in a temp file; the binary writes its outputs into `<dir>/<name>/`). The binary contract is documented in a new file `docs/src/extending/external-generators.md`: stdin/stdout protocol, exit codes, expected directory layout, version negotiation (the binary must accept `--abi-version` and respond with the supported IR schema versions). Add a test `external_generator_discovery_finds_path_binary` (create a temp script `weaveffi-gen-test` in a tempdir, prepend to `PATH`, run weaveffi with `--target test`, assert the script was invoked). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add an `--external-only` flag and a `weaveffi list-generators` subcommand. The list shows built-in generators, then `[external]` entries with their paths and supported IR versions. Add a test `list_generators_includes_external`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 35 — Template engine: actually wire generators

`--templates <dir>` and the `template_dir` config field exist but no
generator overrides `generate_with_templates`. Wire at least three generators
so the feature is real.

- [ ] Wire the C generator's header generation through Tera. In `crates/weaveffi-gen-c/src/lib.rs`, override `generate_with_templates`. If `templates` is `Some` and contains a template named `c/header.tera`, use it to render the header instead of the built-in formatter. Build the Tera context using `templates::api_to_context(api)`. Provide built-in templates as compile-time `include_str!`-ed constants (in a new `templates/` directory inside the crate, registered via `TemplateEngine::load_builtin`). User templates loaded via `load_dir` override the built-ins. Add a test `c_user_template_overrides_builtin` that creates a tempdir, writes a custom `c/header.tera` (e.g., changes the comment style), invokes generate with `--templates <tmpdir>`, and asserts the output reflects the custom template. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Wire the Swift generator through Tera. Same as the C generator but for `swift/wrapper.tera`. Add a test `swift_user_template_overrides_builtin`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Wire the Python generator through Tera. Same as the C generator but for `python/module.tera` and `python/stubs.tera`. Add a test `python_user_template_overrides_builtin`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Document the template extension model in `docs/src/extending/templates.md`. Cover: built-in template names per generator, Tera context schema (`api`, `modules`, `functions`, `structs`, `enums`, `callbacks`, `listeners`), available filters (e.g., `to_camel_case`, `to_snake_case`), and provide a worked example "customise the C header to use Doxygen comments". Run `cargo test --workspace` to verify nothing is broken.

### Phase 36 — Extract command: feature parity with hand-written IDL

The `extract` command misses many features the hand-written IDL supports.
Bring them to parity.

- [ ] Add typed handle extraction. In `crates/weaveffi-cli/src/extract.rs` `map_type`, recognise types annotated as `weaveffi_handle::Handle<MyStruct>` (or a `#[weaveffi_typed_handle = "MyStruct"]` attribute) and emit `TypeRef::TypedHandle("MyStruct")`. Add a test `extract_typed_handle_param`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add borrowed type extraction. Recognise `&str` → `TypeRef::BorrowedStr` and `&[u8]` → `TypeRef::BorrowedBytes`. Add tests `extract_borrowed_str_param` and `extract_borrowed_bytes_param`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add async / cancellable / deprecated / since extraction. For `#[weaveffi_export]` functions, recognise the attribute extras: `#[weaveffi_export(async)]` sets `r#async = true`, `#[weaveffi_export(cancellable)]` sets `cancellable = true`, `#[deprecated(note = "...")]` populates `deprecated`, `#[weaveffi_export(since = "0.5.0")]` populates `since`. Add tests `extract_async_function`, `extract_cancellable_function`, `extract_deprecated_function`, `extract_since_attribute`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add iterator and callback extraction. Recognise `impl Iterator<Item = T>` return types as `TypeRef::Iterator(T)`. Recognise `Box<dyn Fn(...) -> ...>` parameters and require a `#[weaveffi_callback = "Name"]` attribute on the parameter to map to `TypeRef::Callback("Name")`. Recognise top-level `#[weaveffi_callback]` types as `CallbackDef` definitions. Add tests `extract_iterator_return`, `extract_callback_param_with_attribute`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add builder extraction. For `#[weaveffi_struct(builder)]` set `StructDef::builder = true`. Add a test `extract_struct_with_builder_attribute`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add struct field default extraction. For struct fields annotated with `#[weaveffi_default = "<yaml-literal>"]` populate `StructField::default` with the parsed YAML value. Add a test `extract_struct_field_default`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add nested module and listener extraction. Recognise `mod` blocks inside other `mod` blocks as `Module::modules`. Recognise `#[weaveffi_listener(event = "OnX")]` items as `ListenerDef`. Add tests `extract_nested_modules`, `extract_listener`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add non-`i32` repr enum extraction. Recognise `#[repr(u8)]` / `#[repr(u32)]` / `#[repr(i64)]` enums and either reject (unsupported) with a clear error, or accept and map via a future IR extension `EnumDef::repr`. Choose: reject for now and document in the extract guide. Add a test `extract_enum_with_unsupported_repr_rejected`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update the extract docs. Update `docs/src/guides/extract.md` to document every supported attribute, a complete example matching the contacts sample's hand-written IDL, and a list of limitations (no generic functions, no trait impls, no lifetime parameters except in `&str`/`&[u8]`). Run `cargo test --workspace` to verify nothing is broken.

### Phase 37 — Doctor: comprehensive toolchain checks

`weaveffi doctor` checks rustc/cargo/xcodebuild/ndk/node and target installation,
but production use needs more: clang, swiftc, ndk-bundle path, dotnet, dart,
go, ruby, gem, pkg-config, etc.

- [ ] Expand `weaveffi doctor` checks. In `crates/weaveffi-cli/src/main.rs` `cmd_doctor`, add checks for: `clang`/`gcc` (C/C++ compilation), `cmake`, `swiftc` (macOS), `swift package` (SwiftPM), `dotnet` (>= 8.0), `dart` (>= 3.0), `flutter` (optional), `go` (>= 1.21), `ruby` (>= 3.0), `gem`, `bundler`, `python3` (>= 3.8), `pip`, `node` (>= 18), `npm`, `node-gyp`. Group output by target. For each missing tool emit a hint with the install command (`brew install ...` on macOS, `apt install ...` on Debian/Ubuntu, `winget install ...` on Windows, `dnf install ...` on Fedora). Add a `--target <names>` flag to limit checks to specific targets. Add a test `doctor_checks_per_target_toolchains`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 38 — Generated package quality: Swift

- [ ] Polish the generated SwiftPM package. In `crates/weaveffi-gen-swift/src/lib.rs`, ensure `Package.swift` declares `swiftLanguageVersions: [.v5]`, `platforms: [.macOS(.v12), .iOS(.v15), .tvOS(.v15), .watchOS(.v8), .visionOS(.v1)]`, and the binary target points at a `.xcframework` placeholder under `Frameworks/`. Generate a `Frameworks/README.md` instructing users how to drop in the cross-compiled `.xcframework` (and link to `weaveffi build --xcframework`). Add tests `swift_package_targets_modern_apple_platforms`, `swift_package_declares_xcframework_path`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `--xcframework` mode to `weaveffi build`. When set, after building the cdylib for `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, the command runs `xcodebuild -create-xcframework` to bundle them into `<out>/swift/Frameworks/{name}.xcframework`. Document usage in `docs/src/tutorials/swift.md`. Add a smoke test gated on macOS. Run `cargo test --workspace` to verify nothing is broken.

### Phase 39 — Generated package quality: Android

- [ ] Polish the generated Android Gradle module. In `crates/weaveffi-gen-android/src/lib.rs`, generate a complete `build.gradle` with: `android.compileSdk = 34`, `android.namespace = "{android_package}"`, `android.defaultConfig { minSdk = 21, ndk { abiFilters "arm64-v8a", "armeabi-v7a", "x86_64" } }`, `android.externalNativeBuild.cmake.path = "src/main/cpp/CMakeLists.txt"`, `android.buildFeatures { prefab = true }`. Generate a `consumer-rules.pro` with appropriate ProGuard/R8 keep rules for the JNI classes. Generate a `AndroidManifest.xml` minimal stub. Add tests `android_build_gradle_has_modern_sdk`, `android_has_consumer_rules`, `android_has_manifest`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `--aar` mode to `weaveffi build`. When set, after generating Android bindings and building the cdylib for `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android`, place the `.so` files into `android/src/main/jniLibs/{abi}/lib{name}.so` and run `gradle bundleRelease` to produce an `.aar`. Smoke test gated on Android NDK presence. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Fix the Kotlin `U32` mapping. In `crates/weaveffi-gen-android/src/lib.rs`, the `kotlin_type` mapping for `TypeRef::U32` currently emits `Long`. Change to `UInt` (unsigned 32-bit) and update the JNI bridge to declare the corresponding `jint`/`UInt` boundary. Also audit `I64` (should be `Long`), `U64` if/when added. Add a test `kotlin_u32_maps_to_uint`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 40 — Generated package quality: Node

- [ ] Polish the generated Node package. In `crates/weaveffi-gen-node/src/lib.rs`, the `package.json` should declare `engines.node = ">=18"`, `type = "module"`, `exports = { ".": "./index.js", "./types": "./types.d.ts" }`, `files = ["index.js", "types.d.ts", "weaveffi_addon.c", "binding.gyp", "build/", "*.node"]`, `scripts.install = "node-gyp rebuild"` (already present), `scripts.test = "node --test"`. Add a `LICENSE` file copy step (require the user to drop one in). Add tests `node_package_json_has_engines`, `node_package_json_has_exports`, `node_package_json_lists_files`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Generate a `.npmignore` that excludes `target/`, `*.rs`, `Cargo.toml`, `node_modules/`, `.git/`, `build/intermediates/`. Add a test `node_generates_npmignore`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add prebuilt-binary support hooks. Add to `package.json` a `binary` block compatible with `node-pre-gyp` or `prebuildify` so users can ship prebuilt addons for their consumers without requiring a C toolchain. Document the workflow in `docs/src/tutorials/node.md`. Add a test `node_package_json_has_prebuild_hooks`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 41 — Generated package quality: Python

- [ ] Polish the generated Python package. In `crates/weaveffi-gen-python/src/lib.rs`, `pyproject.toml` should declare `requires-python = ">=3.8"`, `[build-system] requires = ["setuptools>=61", "wheel"]`, `[project.optional-dependencies] dev = ["pytest", "mypy"]`, `[tool.setuptools.package-data]` to include the cdylib `.dylib`/`.so`/`.dll` in the wheel, and `[tool.setuptools.dynamic.version]` reading from `__init__.py`. Add a `MANIFEST.in` listing the native libraries. Generate a `tests/__init__.py` and `tests/test_smoke.py` skeleton that imports the module and calls a simple function. Add tests `python_pyproject_has_modern_metadata`, `python_includes_native_lib_in_package_data`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Generate a `wheel`-friendly setup. Add `cibuildwheel` config (`[tool.cibuildwheel]` in `pyproject.toml` listing Linux/macOS/Windows target wheels, plus a `before-build` hook to run `weaveffi build`). Document in `docs/src/tutorials/python.md` how to build wheels for upload to PyPI. Run `cargo test --workspace` to verify nothing is broken.

### Phase 42 — Generated package quality: .NET

- [ ] Polish the generated NuGet package. In `crates/weaveffi-gen-dotnet/src/lib.rs`, the `.csproj` should: `<TargetFramework>net8.0</TargetFramework>`, `<Nullable>enable</Nullable>`, `<TreatWarningsAsErrors>true</TreatWarningsAsErrors>`, `<GeneratePackageOnBuild>false</GeneratePackageOnBuild>`. The `.nuspec` should include a `<files>` element bundling the native libraries under `runtimes/{rid}/native/lib{name}.{ext}` (where rid is `linux-x64`, `linux-arm64`, `osx-x64`, `osx-arm64`, `win-x64`, etc.). Add a `runtimes/` directory placeholder with a README explaining where to drop in cross-compiled binaries. Add tests `dotnet_csproj_has_modern_settings`, `dotnet_nuspec_includes_native_runtimes`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 43 — Generated package quality: Dart, Go, Ruby, C++

- [ ] Polish the Dart pubspec. In `crates/weaveffi-gen-dart/src/lib.rs`, `pubspec.yaml` should declare `environment.sdk: ">=3.0.0 <4.0.0"`, `environment.flutter: ">=3.10.0"` (optional), `dependencies.ffi: ^2.1.0`, `dev_dependencies.test: ^1.24.0`. Add a `lib/src/` folder with internal-only declarations, and `lib/{package}.dart` as the public exports barrel file. Generate `analysis_options.yaml` enabling `package:flutter_lints/flutter.yaml`. Add tests `dart_pubspec_has_modern_sdk_constraints`, `dart_has_analysis_options`, `dart_has_barrel_export`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Polish the Go module. In `crates/weaveffi-gen-go/src/lib.rs`, `go.mod` should declare a real module path matching the user's `go_module_path` config (default `github.com/example/weaveffi`). Generate a `go.sum` placeholder. Generate a `doc.go` file with package-level documentation. Generate a `weaveffi_test.go` smoke test skeleton. Add tests `go_mod_uses_module_path`, `go_has_doc_go`, `go_has_smoke_test`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Polish the Ruby gem. In `crates/weaveffi-gen-ruby/src/lib.rs`, `weaveffi.gemspec` should declare `s.required_ruby_version = ">= 3.0"`, `s.metadata = { "source_code_uri" => ..., "documentation_uri" => ... }`, `s.add_runtime_dependency "ffi", "~> 1.16"`, `s.add_development_dependency "rspec", "~> 3.12"`. Generate a `Gemfile` and `Gemfile.lock` placeholder, plus `spec/{gem_name}_spec.rb` smoke test. Add tests `ruby_gemspec_has_metadata`, `ruby_has_gemfile`, `ruby_has_smoke_spec`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Polish the C++ CMake setup. In `crates/weaveffi-gen-cpp/src/lib.rs`, generate a `CMakeLists.txt` with `cmake_minimum_required(VERSION 3.16)`, support for `find_package({c_prefix})`, and provide a `{c_prefix}-config.cmake.in` template installable via `install(EXPORT ...)`. Generate a `vcpkg.json` and `conanfile.py` for vcpkg / Conan distribution. Add tests `cpp_cmake_supports_find_package`, `cpp_has_vcpkg_json`, `cpp_has_conanfile`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 44 — Calculator sample: tests, README, documentation alignment

- [ ] Add unit tests to the calculator sample. In `samples/calculator/src/lib.rs`, add a `#[cfg(test)] mod tests` block with: `add_works`, `mul_works`, `div_by_zero_returns_error`, `echo_round_trips_utf8`, `echo_round_trips_empty_string`, `echo_handles_long_string` (10 KB). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `samples/calculator/README.md` covering: what the sample demonstrates, how to generate bindings with each target, how to build the cdylib, and how to run the C example end-to-end. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Repeat the README addition for `samples/contacts`, `samples/inventory`, `samples/async-demo`, `samples/events`, and `samples/node-addon`. Each README should explain: what features the sample demonstrates, the IDL highlights, the generate command, and a "what to look for in the generated output" section. Run `cargo test --workspace` to verify nothing is broken.

### Phase 45 — Persistence-backed sample (SQLite contacts)

A real-world sample demonstrating non-trivial state and async I/O.

- [ ] Create a SQLite-backed contacts sample. Create `samples/sqlite-contacts/` as a workspace member. `Cargo.toml` depends on `weaveffi-abi`, `rusqlite = { version = "0.31", features = ["bundled"] }`. Create `samples/sqlite-contacts/sqlite_contacts.yml` with a `contacts` module containing: enum `Status { Active, Archived }`, struct `Contact { id: i64, name: string, email: string?, status: Status, created_at: i64 }`, async functions `create_contact(name: string, email: string?) -> Contact` (cancellable), `find_contact(id: i64) -> Contact?`, `list_contacts(status: Status?) -> [Contact]` (returns iter via `iter<Contact>`), `update_contact(id: i64, email: string?) -> bool`, `delete_contact(id: i64) -> bool`, `count_contacts(status: Status?) -> i64`. Implement all functions in `samples/sqlite-contacts/src/lib.rs` using a connection pool. Use `tokio::task::spawn_blocking` (add `tokio` dependency with `rt-multi-thread` features) to run the SQLite calls and invoke the C callback when done. Add tests `crud_round_trip`, `iterator_returns_all_contacts`, `cancel_during_long_query_returns_cancelled`. Add the sample to workspace members. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `samples/sqlite-contacts/README.md` explaining: this is a real-world reference; the SQLite database is created in a tempdir on first call; the sample demonstrates async + cancellation + iterators + optionals + structs + enums in a non-trivial setting. Show how to generate Python and Swift bindings and what the resulting consumer code looks like. Run `cargo test --workspace` to verify nothing is broken.

### Phase 46 — Networking-backed sample (HTTP fetch)

- [ ] Create an HTTP-fetch sample. Create `samples/http-fetch/` as a workspace member. `Cargo.toml` depends on `weaveffi-abi`, `reqwest = { version = "0.12", features = ["rustls-tls", "json"], default-features = false }`, `tokio = { version = "1", features = ["rt-multi-thread"] }`, `serde_json = "1"`. Create `samples/http-fetch/http_fetch.yml` with module `http` containing: enum `HttpMethod { Get, Post, Put, Delete }`, struct `HttpResponse { status: i32, body: bytes, headers: {string:string} }`, async function `fetch(url: string, method: HttpMethod, body: bytes?, timeout_ms: i32) -> HttpResponse` (cancellable). Implement in `samples/http-fetch/src/lib.rs`. Add tests `fetch_get_works_against_local_server` (use `wiremock` test crate). Add to workspace. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a `samples/http-fetch/README.md`. Document the security implications (rustls vs system openssl), how to override the user-agent, and example consumer code in Swift/Kotlin/Python. Run `cargo test --workspace` to verify nothing is broken.

### Phase 47 — Examples for C++ target

- [ ] Create `examples/cpp/` directory with a working CMake project. Create `examples/cpp/contacts/` with `CMakeLists.txt`, `main.cpp`, `README.md`. The `main.cpp` includes the generated `weaveffi.hpp`, creates contacts via the C++ wrapper, lists them, demonstrates RAII cleanup. The `CMakeLists.txt` does `add_subdirectory(../../generated/cpp ../weaveffi_cpp_build)` and `target_link_libraries(contacts PRIVATE weaveffi_cpp ${CMAKE_DL_LIBS})` plus a hint to set `LD_LIBRARY_PATH` for the cdylib. The README has step-by-step build/run on macOS/Linux/Windows. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `examples/cpp/calculator/` similarly for the calculator sample. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `examples/cpp/sqlite-contacts/` demonstrating the async + cancellation + iterator features against the SQLite sample, using `std::future::wait_for(timeout)` to drive cancellation. Run `cargo test --workspace` to verify nothing is broken.

### Phase 48 — Examples for Dart target

- [ ] Create `examples/dart/contacts/` with `bin/main.dart`, `pubspec.yaml`, `README.md`. The `main.dart` imports the generated `package:weaveffi/weaveffi.dart`, runs through the contacts CRUD demo, demonstrates `dispose()` (the Dart equivalent of RAII). The README shows how to run with `dart pub get && dart run`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `examples/dart/sqlite-contacts/` showing async/await with the SQLite sample. Run `cargo test --workspace` to verify nothing is broken.

- [ ] (Optional) Create `examples/dart/flutter-contacts/` — a minimal Flutter app that uses the bindings to render a contacts list. Mark as optional in CI (only build if Flutter SDK available). Run `cargo test --workspace` to verify nothing is broken.

### Phase 49 — Examples for Go target

- [ ] Create `examples/go/contacts/` with `main.go`, `go.mod`, `README.md`. The `main.go` imports the generated module (`replace` directive points at `../../generated/go`), runs the CRUD demo, demonstrates explicit `Close()` per struct. README shows how to set `CGO_LDFLAGS` to point at the cdylib build directory. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `examples/go/sqlite-contacts/` showing channel-based async + iterator consumption (`for contact := range list_contacts() { ... }`). Run `cargo test --workspace` to verify nothing is broken.

### Phase 50 — Examples for Ruby target

- [ ] Create `examples/ruby/contacts/` with `Gemfile`, `bin/contacts.rb`, `README.md`. The script requires the generated gem, runs the CRUD demo, demonstrates Ruby's `FFI::AutoPointer` cleanup. README shows how to set `LD_LIBRARY_PATH` and run with `bundle exec`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `examples/ruby/sqlite-contacts/` showing block-based async (`fetch_async(url) { |result| ... }`) and Enumerator-based iterator consumption. Run `cargo test --workspace` to verify nothing is broken.

### Phase 51 — Complete WASM examples

- [ ] Replace the partial `examples/wasm/` with a complete browser example. `examples/wasm/browser/` contains `index.html`, `app.js` (imports the generated `weaveffi_wasm.js`, instantiates with a worker), `serve.sh` (a `python3 -m http.server 8080` wrapper), and `README.md`. The HTML has a small UI calling the calculator's `add` and `echo`. Demonstrate error handling. Add a build script that compiles the calculator cdylib for `wasm32-unknown-unknown` first. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `examples/wasm/node/` showing how to load the WASM module from Node 22+ (which has WebAssembly support without flags) and call functions. Include `package.json`, `index.mjs`, `README.md`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `examples/wasm/contacts/` demonstrating the full async + iterator + struct-handle feature set against the SQLite sample compiled to wasi-preview-2 (or, for now, document that complex samples requiring SQLite are not yet supported on `wasm32-unknown-unknown` and use the calculator sample for the WASM contacts demo). Run `cargo test --workspace` to verify nothing is broken.

### Phase 52 — Runnable Android example

- [ ] Replace the template-only `examples/android/` with a complete Android Studio project. Create `examples/android/contacts-app/` containing `app/src/main/AndroidManifest.xml`, `app/src/main/java/com/example/contacts/MainActivity.kt`, `app/src/main/res/layout/activity_main.xml`, `app/build.gradle`, `settings.gradle`, `gradle.properties`, `gradlew`, `gradlew.bat`, `gradle/wrapper/gradle-wrapper.properties`, plus the generated bindings `app/src/main/jniLibs/{abi}/lib{name}.so` placeholder. The activity displays a list of contacts using the generated wrapper. Add a `README.md` with build instructions: `cd examples/android/contacts-app && ./gradlew assembleDebug`. Cross-compile the cdylib for `aarch64-linux-android`/`armv7-linux-androideabi`/`x86_64-linux-android` and place the `.so` files in `jniLibs/{abi}/`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 53 — Fix Node contacts example

The current `examples/node/contacts.mjs` references a generated contacts API,
but `samples/node-addon` only loads `weaveffi_calculator_*` symbols, so the
example is broken.

- [ ] Replace `samples/node-addon` with a more general loader that uses `libloading` to dynamically load any cdylib at runtime via the `WEAVEFFI_LIB` environment variable, then exposes ALL `weaveffi_*` symbols from the loaded library via reflection. Or alternatively, generate the N-API addon directly from the contacts IDL and ship a fully-built `index.node` for both calculator AND contacts. Choose the latter: rename `samples/node-addon` to `samples/node-addon-calculator` and add `samples/node-addon-contacts` for the contacts sample. Update `examples/node/contacts.mjs` to use the contacts addon. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update CI to build and run the Node contacts example end-to-end alongside the calculator example. In `.github/workflows/ci.yml`, add steps after the existing Node calculator smoke run: generate contacts bindings, build the contacts cdylib + node addon, copy the addon, run `node examples/node/contacts.mjs`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 54 — Fix `examples/c/contacts.c` duplication

- [ ] Audit `examples/c/contacts.c` and remove any redundantly-redeclared C ABI function prototypes — rely entirely on `#include "weaveffi.h"`. If the duplications were workarounds for build issues, fix the build instead. Add a CI step that compiles the example with `-Wmissing-prototypes -Wstrict-prototypes -Werror` to keep this fixed. Run `cargo test --workspace` to verify nothing is broken.

### Phase 55 — Documentation: fix stale content

- [ ] Update `docs/src/intro.md` to list all 11 supported target languages (currently lists only 5). Use the same table format as the README. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update `docs/src/samples.md` to remove the "validator rejects async" claim from the async-demo section. Add the new SQLite-contacts and HTTP-fetch sample sections per Phases 45 and 46. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update `docs/src/reference/naming.md` to say crates ARE published (link to crates.io and npm) and remove the "Planned package names (not yet published)" wording. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Update `docs/src/getting-started.md` to use `weaveffi-abi = "0.2"` (the current published version), and after Phase 95 (1.0 release prep) bump to `1` (semver caret). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Unify the docs URL. Choose one canonical URL: `https://docs.weaveffi.com/`. Replace every `weavefoundry.github.io/weaveffi` reference in `docs/src/api/README.md`, `docs/src/api/rust.md`, `README.md`, and any other markdown file with `https://docs.weaveffi.com/...`. Add a redirect note in `docs/book.toml` if needed. Add a CI step that greps for `weavefoundry.github.io` in the docs and fails if found. Run `cargo test --workspace` to verify nothing is broken.

### Phase 56 — Documentation: SECURITY, FAQ, troubleshooting

- [ ] Create a `SECURITY.md` at the repo root. Cover: how to report a security issue (security@weavefoundry.example or a private GitHub Security Advisory), supported versions (1.0.x will be supported for 12 months after release), the threat model (WeaveFFI generates code that runs in user processes; the CLI itself does not execute generated code), known-safe assumptions (we trust the input IDL author), and a list of past CVEs (empty). Add a link from the `README.md`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/security.md` mirroring `SECURITY.md` and add to `docs/src/SUMMARY.md`. Cover the same topics plus: memory safety guarantees of generated code (we audit per-target), the `unsafe_code = deny` workspace lint, `cargo-audit` running in CI, and SBOM publication. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/troubleshooting.md`. Cover: common errors when generating (missing toolchains, IDL parse errors), common errors when building generated bindings (linker errors, library not found, version mismatch with `weaveffi-abi`), common errors when running (`Symbol not found`, `dlopen failed`, ABI mismatch), platform-specific gotchas (macOS Gatekeeper signing, Linux GLIBC versioning, Windows MSVC vs MinGW). Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/faq.md`. Q&A format covering: "Can I use WeaveFFI with C++ instead of Rust?" "Can I use it with Zig?" "How do I version my IDL?" "Do I need to ship the WeaveFFI runtime to consumers?" "How do I handle thread safety?" "How do I unit-test generated bindings?" "Why a stable C ABI instead of $other-IPC?" "How is WeaveFFI different from UniFFI / cbindgen / cxx?" Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

### Phase 57 — Documentation: deployment guide

- [ ] Create `docs/src/guides/deployment.md`. Cover: shipping a Rust cdylib to npm (with prebuilt binaries via `prebuildify`), to PyPI (`cibuildwheel`), to Maven Central (Android library AAR), to NuGet (with `runtimes/{rid}/native/`), to crates.io (the runtime crate), to RubyGems (with native extension or pre-bundled `.so`), to pub.dev (with FFI plugin pattern), to vcpkg/Conan (C++), to a Helm chart for sidecar use. Each section has the recommended CI workflow snippet. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

### Phase 58 — Documentation: CHANGELOG and CONTRIBUTING in mdBook

- [ ] Add CHANGELOG to mdBook. Create `docs/src/changelog.md` with `{{#include ../../CHANGELOG.md}}` (mdBook supports include directives). Add to SUMMARY.md. Verify the include works locally. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add CONTRIBUTING to mdBook. Create `docs/src/contributing.md` with `{{#include ../../CONTRIBUTING.md}}`. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add LICENSE info to mdBook. Create `docs/src/license.md` linking to both LICENSE-MIT and LICENSE-APACHE files in the repo. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

### Phase 59 — Documentation: per-target tutorials for missing targets

PRD-v3 added Swift, Android, Python, Node tutorials. Add the missing ones.

- [ ] Create `docs/src/tutorials/cpp.md` showing a complete walk-through: define IDL, run generate, integrate with CMake, build a small C++ app that uses RAII, demonstrate exception handling, demonstrate `std::future` async, link instructions for macOS/Linux/Windows. Add to SUMMARY.md under Tutorials. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/tutorials/dart.md` covering: `dart pub get`, `dart:ffi` setup, async/await, Flutter integration tip, distribution via pub.dev. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/tutorials/go.md` covering: cgo setup, `go build` flags, channel-based async, distribution via `go install`, vendoring native binaries via `embed`. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/tutorials/ruby.md` covering: `bundle install`, FFI gem usage, block-based async, native extension distribution via `gem`, common pitfalls (`LD_LIBRARY_PATH` vs `RPATH`). Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/tutorials/dotnet.md` covering: `dotnet new`, P/Invoke setup, async Task usage, NuGet packing with native runtimes, distribution. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Create `docs/src/tutorials/wasm.md` covering: building the cdylib for `wasm32-unknown-unknown`, hosting the `.wasm` file, instantiation in browser vs Node, async + Promise integration, error handling, current limitations (no SQLite, no networking on plain wasm32-unknown-unknown). Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

### Phase 60 — Documentation: book.toml and SUMMARY hygiene

- [ ] Set `create-missing = false` in `docs/book.toml` so missing chapters fail the docs build instead of creating empty stubs. Run `mdbook build docs` locally and ensure it still succeeds. Add a CI step that runs `mdbook build docs` and `mdbook test docs` and fails on any error. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add an mdBook link checker step. Add `mdbook-linkcheck = "0.7"` as a docs build prerequisite (install via cargo in CI or via `peaceiris/actions-mdbook` extension). Add `[output.linkcheck]` to `docs/book.toml`. The CI docs job must fail if any internal link is broken. Run `cargo test --workspace` to verify nothing is broken.

### Phase 61 — Crate metadata polish

- [ ] Add `rust-version = "1.74"` to every `crates/*/Cargo.toml` `[package]` section (or to `[workspace.package]` and inherit). Choose the MSRV by running `cargo install cargo-msrv` and checking each crate. Update the workspace `rust-toolchain.toml` to remain on `stable` but document the MSRV in `CONTRIBUTING.md`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `readme = "README.md"`, `documentation = "https://docs.weaveffi.com/"`, `homepage = "https://weaveffi.com"` to every `crates/*/Cargo.toml` `[package]` section. For crates that don't have their own README, create a minimal one referencing the workspace README. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add per-crate `keywords` and `categories` audit. Each generator crate should have `keywords` of length 5 max and exactly one of: `["development-tools::ffi", "development-tools::build-utils"]`. The CLI crate should add `["command-line-utilities"]`. Verify `cargo publish --dry-run -p <each>` passes. Run `cargo test --workspace` to verify nothing is broken.

### Phase 62 — `weaveffi-runtime` consumer-facing crate

The current `weaveffi-abi` crate is for the runtime symbols (allocator,
error, cancel token). Consumers need to depend on it from their cdylib
implementations. Make this story explicit by ensuring `weaveffi-abi` has
clean consumer-facing docs and consider renaming for clarity.

- [ ] Audit `weaveffi-abi` for consumer-friendliness. In `crates/weaveffi-abi/src/lib.rs`, ensure every public item has rustdoc with: a one-line summary, parameter docs, safety notes (most items are `unsafe extern "C"` so `# Safety` sections are required), examples. Add `#![deny(missing_docs)]` to enforce. Add a `crates/weaveffi-abi/README.md` covering: this crate is what your cdylib depends on; it provides `weaveffi_error`, `weaveffi_alloc`, `weaveffi_free`, `weaveffi_free_string`, `weaveffi_free_bytes`, `weaveffi_cancel_token`, and `weaveffi_arena_*` symbols; here's a minimal `lib.rs` skeleton. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Decide and apply a rename if useful. Rename `weaveffi-abi` to `weaveffi-runtime` (more descriptive of its consumer role) using `cargo rename` or manual edits. Update all dependents (every sample's `Cargo.toml`, every generator crate's docs, the scaffold, the docs). Bump major version since this is a breaking change. Update `scripts/publish-crates.sh`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 63 — Determinism: stable iteration order

Codegen output must be byte-identical across runs given the same input.
HashMaps with non-deterministic iteration order can break this.

- [ ] Audit every generator for `HashMap` iteration in output paths. Replace with `BTreeMap` or with `Vec<(K, V)>` sorted on insertion. Specifically check: per-module function/struct/enum iteration (already deterministic via `Vec`), but anywhere a `HashMap` is created for fast lookup (e.g., the type resolution map in `validate.rs`) and then iterated for output, sort first. Add a determinism test in each generator: `{generator}_output_is_deterministic` that generates the contacts API twice in two separate temp dirs and asserts every file is byte-identical. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a workspace-wide determinism test. In `crates/weaveffi-cli/tests/determinism.rs`, test `weaveffi_generate_is_deterministic` that runs `weaveffi generate samples/contacts/contacts.yml -o <a>` and `... -o <b>` in two tempdirs and asserts every file is byte-identical. Run `cargo test --workspace` to verify nothing is broken.

### Phase 64 — Capability-aware help text and target descriptions

Make `weaveffi --help` and `weaveffi generate --help` informative.

- [ ] Update CLI help text in `crates/weaveffi-cli/src/main.rs`. The `--target` help text should list all available targets with one-line descriptions auto-generated from each generator's `description() -> &str` method (add to the `Generator` trait). The CLI's main `--help` should include a "Quick start" section: `weaveffi new myproj`, `cd myproj`, `weaveffi generate weaveffi.yml`. Add a test `help_lists_all_targets`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `description()` to the `Generator` trait with a default of "{name} bindings" and override in each generator with a short, informative string. Run `cargo test --workspace` to verify nothing is broken.

### Phase 65 — Validation suggestions test completeness

The existing `validation_suggestion_covers_all_variants` test omits some
variants. Make it actually cover all of them.

- [ ] Fix the validation suggestion test in `crates/weaveffi-cli/src/main.rs` (or wherever the test lives). Use a static enum-iteration helper (manual `match` on every variant of `ValidationError`) and assert that every variant has a non-empty suggestion. The compiler will error if a new variant is added without updating both the suggestion table and this test. Run `cargo test --workspace` to verify nothing is broken.

### Phase 66 — Capability-aware Doctor target check

- [ ] Make `weaveffi doctor` report each target's required toolchain availability. Add a `target_requirements()` method to the `Generator` trait returning a list of `&'static str` toolchain names (e.g., Swift returns `["swiftc"]`, Android returns `["javac", "aarch64-linux-android"]`). Doctor checks each named tool / target. Add a test `doctor_reports_per_target_toolchain_status`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 67 — CLI: progress indicator for long-running generates

For very large IDL files, `weaveffi generate` is silent until done.

- [ ] Add a progress indicator. Add `indicatif = "0.17"` as a workspace dependency. In `crates/weaveffi-cli/src/main.rs` `cmd_generate`, when `--quiet` is not set and the terminal is a TTY (`std::io::IsTerminal`), display a spinner with the current generator name. After each generator completes, increment a progress bar. Disable the spinner when stdout is not a TTY (so logs are clean in CI). Add a test `progress_disabled_when_not_tty`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 68 — CLI: `--out` default to current directory + sensible defaults

- [ ] Audit CLI defaults. The `--out` default for `generate` is `./generated`. Make this configurable via the IDL's `[generators] out = "..."` section or a top-level config option. The default for `validate` and `lint` should be the current directory. The default for `extract` should write to stdout if no `--output` given. Add tests for each default. Run `cargo test --workspace` to verify nothing is broken.

### Phase 69 — CI: MSRV pin and matrix

- [ ] Declare an MSRV (Minimum Supported Rust Version). After Phase 61 added `rust-version = "1.74"`, pin in `.github/workflows/ci.yml` an additional matrix entry running tests on the MSRV. Use `dtolnay/rust-toolchain@1.74` (or whichever pinned version). The MSRV job runs `cargo build --workspace` and `cargo test --workspace` only (no clippy/fmt). Document in `CONTRIBUTING.md` that PRs must not bump the MSRV without prior discussion. Add `cargo-msrv` to the `justfile` as `just msrv` (`cargo install cargo-msrv && cargo msrv verify`). Run `cargo test --workspace` to verify nothing is broken.

### Phase 70 — CI: cross-compilation matrix

- [ ] Add a cross-compilation CI job. Create `.github/workflows/cross.yml` with a matrix over `target: [aarch64-apple-ios, aarch64-linux-android, armv7-linux-androideabi, x86_64-linux-android, wasm32-unknown-unknown, aarch64-unknown-linux-gnu, armv7-unknown-linux-gnueabihf, x86_64-pc-windows-gnu]`. For each target install the toolchain via `rustup target add`, then run `cargo build --target $target -p calculator -p contacts --no-default-features` (samples must compile for all platforms). Use `cross` for non-host architectures. Allow the job to be optional (`continue-on-error: true` per matrix entry) so a single broken target doesn't block PR merges, but the job is still visible. Run `cargo test --workspace` to verify nothing is broken.

### Phase 71 — CI: full Windows E2E

The current `windows-e2e` job in `ci.yml` only verifies file presence.

- [ ] Extend `windows-e2e` to compile and run the C example on Windows. After generating bindings, run `cl /I generated/c examples/c/main.c target/debug/calculator.lib /Fe:c_example.exe`. Then run `c_example.exe`. Use the MSVC toolchain (`cl.exe` is provided by GitHub-hosted runners' Visual Studio install). Add a step that builds and runs the Node addon on Windows. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Extend `windows-e2e` to build and run the Python example on Windows. Install Python 3.12, generate Python bindings, build the calculator cdylib for `x86_64-pc-windows-msvc`, copy `calculator.dll` into `generated/python/weaveffi/`, run `python examples/python/contacts.py` (after the Phase 36 fix to make the script work). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add Windows .NET integration. Build the .NET wrapper, run `dotnet test` against the generated `.cs` against the calculator cdylib. Run `cargo test --workspace` to verify nothing is broken.

### Phase 72 — CI: per-target sample matrix

- [ ] Create `.github/workflows/samples.yml` running per-target end-to-end tests. Matrix over `(target, sample)` for all 11 targets and 4 samples (calculator, contacts, sqlite-contacts, http-fetch). For each combination: install the toolchain, generate bindings, build the cdylib, build the consumer example, run it, capture exit code. Tag flaky combinations with `continue-on-error: true` and document them as known limitations. Run `cargo test --workspace` to verify nothing is broken.

### Phase 73 — CI: Dependabot

- [ ] Add `.github/dependabot.yml` with: cargo daily for the main workspace; npm weekly for `package.json`; github-actions weekly for `.github/workflows/`. Group all minor + patch updates into one PR per ecosystem to reduce noise. Configure auto-rebase and auto-assignment to a maintainer. Run `cargo test --workspace` to verify nothing is broken.

### Phase 74 — CI: CodeQL

- [ ] Add `.github/workflows/codeql.yml` running CodeQL analysis on push to main and weekly. Languages: `rust` (via the `github/codeql-action` Rust beta or `cargo` analysis), `javascript` (for the generated Node wrappers we ship in samples), `python` (for examples), `java` (for Android). Configure to upload results to GitHub Security. Run `cargo test --workspace` to verify nothing is broken.

### Phase 75 — CI: cargo-audit and cargo-deny

- [ ] Add a `.github/workflows/audit.yml` running `cargo-audit` on every push. Use `rustsec/audit-check` action. Fail on any unfixed vulnerability. Run weekly via cron in addition to on push. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `cargo-deny` config in `deny.toml` at the repo root. Configure: license allow-list (`["MIT", "Apache-2.0", "MIT OR Apache-2.0", "Unicode-DFS-2016", "Unicode-3.0", "BSD-3-Clause", "BSD-2-Clause", "ISC", "Zlib", "MPL-2.0"]`), source allow-list (only crates.io, no git deps), advisory check (deny RUSTSEC for unmaintained crates), bans (`prefer-multiple-versions = "deny"` to keep dep tree clean). Add `.github/workflows/deny.yml` running `cargo deny check` on every push. Run `cargo test --workspace` to verify nothing is broken.

### Phase 76 — CI: benchmark gates

The Criterion benchmarks were added in PRD-v3 but never run in CI.

- [ ] Add `.github/workflows/bench.yml` running `cargo bench --workspace -- --save-baseline pr` on every push to a PR, then comparing to `--baseline main` (run on main pushes, stored as a GitHub artifact via `benchmark-action/github-action-benchmark`). Open a PR comment if any benchmark regresses by >10%. Run `cargo test --workspace` to verify nothing is broken.

### Phase 77 — CI: docs link checker and fail-on-warnings

- [ ] Update `.github/workflows/docs.yml` to fail on any rustdoc warning. Add `RUSTDOCFLAGS="-D warnings"` env. Add the mdBook link checker from Phase 60. Run `cargo test --workspace` to verify nothing is broken.

### Phase 78 — Release: prebuilt CLI binaries via cargo-dist

- [ ] Adopt `cargo-dist` for prebuilt binary releases. Run `cargo dist init` once locally to generate `dist-workspace.toml`. Configure: targets `[aarch64-apple-darwin, x86_64-apple-darwin, aarch64-unknown-linux-gnu, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, x86_64-pc-windows-msvc, aarch64-pc-windows-msvc]`; installers `[shell, powershell, homebrew, msi]`; auto-tag-prefix `v`; ci `github`. Commit the generated `.github/workflows/release.yml` (it integrates with existing semantic-release; configure semantic-release to call `cargo dist build` after `cargo publish`). Test with a dry-run release by manually triggering the workflow on a fork. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Document the install paths in the README. The README's "Install" section should now offer four options: `cargo install weaveffi-cli`, `brew install weavefoundry/tap/weaveffi`, PowerShell installer one-liner, and "download from GitHub Releases". Run `cargo test --workspace` to verify nothing is broken.

### Phase 79 — Release: Homebrew tap

- [ ] Set up a Homebrew tap via cargo-dist. The cargo-dist Homebrew installer publishes formulas to a `homebrew-tap` repo under the `weavefoundry` org. Verify the tap is created and the formula passes `brew audit --strict --online weaveffi`. Add a CI step that, after release, also runs `brew test weavefoundry/tap/weaveffi` on macOS to verify the formula installs. Run `cargo test --workspace` to verify nothing is broken.

### Phase 80 — Release: Scoop bucket

- [ ] Set up a Scoop bucket. cargo-dist supports Scoop via the `scoop` installer. Create `scoop-bucket` repo under `weavefoundry`. Verify the manifest passes `scoop checkup`. Add a CI job using `actions/runner-images` Windows that installs from the Scoop bucket and runs `weaveffi --version`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 81 — Release: .deb and .rpm packages

- [ ] Add `cargo-deb` configuration to `crates/weaveffi-cli/Cargo.toml` `[package.metadata.deb]` with: `maintainer`, `copyright`, `license-file`, `extended-description`, `depends = "$auto"`, `assets = [["target/release/weaveffi", "/usr/bin/weaveffi", "0755"], ["LICENSE-MIT", "/usr/share/doc/weaveffi/LICENSE-MIT", "0644"], ["LICENSE-APACHE", "/usr/share/doc/weaveffi/LICENSE-APACHE", "0644"]]`. Add a CI step in the release workflow that runs `cargo install cargo-deb && cargo deb -p weaveffi-cli` and uploads the `.deb` to the GitHub release. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add `cargo-generate-rpm` configuration to `crates/weaveffi-cli/Cargo.toml` `[package.metadata.generate-rpm]`. Mirror the `.deb` config. Add a CI step running `cargo install cargo-generate-rpm && cargo generate-rpm -p weaveffi-cli` and upload to the release. Run `cargo test --workspace` to verify nothing is broken.

### Phase 82 — Release: Windows MSI installer

- [ ] cargo-dist supports MSI installers via the `msi` installer choice (Phase 78). Verify the MSI is signed (set up `WIX_CODE_SIGNING_CERT` and `WIX_CODE_SIGNING_PASSWORD` GitHub secrets). The CI release workflow generates a signed MSI and uploads to the release. Add a smoke test that downloads and installs the MSI on a Windows runner, runs `weaveffi --version`, then uninstalls. Run `cargo test --workspace` to verify nothing is broken.

### Phase 83 — Release: SBOM publication

- [ ] Generate a Software Bill of Materials (SBOM) on every release. Use `cargo-cyclonedx` (`cargo install cargo-cyclonedx`). Add a CI step to the release workflow: `cargo cyclonedx --format json --output-file weaveffi.cdx.json` and upload the SBOM as a release asset. Document in `SECURITY.md` how to verify the SBOM. Run `cargo test --workspace` to verify nothing is broken.

### Phase 84 — Release: signed binaries via cosign

- [ ] Sign every release artifact with cosign. Add a CI step using `sigstore/cosign-installer@v3` and `cosign sign-blob --output-signature <file>.sig` for each binary, MSI, deb, rpm, SBOM. Upload signatures to the release. Document verification in the README install section: `cosign verify-blob --certificate ... --signature ...`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 85 — Release: nightly artifacts

- [ ] Add `.github/workflows/nightly.yml` that runs every night at 00:00 UTC, builds the CLI from `main` for all supported platforms, uploads to a "nightly" GitHub Release (overwriting the previous nightly). Document in the README that bleeding-edge users can grab nightly builds from the nightly release. Run `cargo test --workspace` to verify nothing is broken.

### Phase 86 — `weaveffi-runtime` API documentation polish

- [ ] Run `cargo doc --workspace --no-deps --document-private-items` and fix every warning. Most should be fixed by Phase 62's `#![deny(missing_docs)]`. Verify the docs build is clean and the rustdoc index is browsable. Run `cargo test --workspace` to verify nothing is broken.

### Phase 87 — Generator parity test matrix

- [ ] Add a comprehensive parity test matrix in `crates/weaveffi-cli/tests/parity.rs`. For every combination of (TypeRef variant, generator), assert the generator produces a known-good output snippet for that type. Use a fixture-based approach: for each TypeRef construct a one-function API, generate, and snapshot-test the relevant per-target output snippet. Use the `insta` crate (`cargo install cargo-insta`) for snapshot management. The matrix should have ~150 cells (15 TypeRef variants × 11 generators, minus invalid combinations like `Iterator` as a parameter which is forbidden). Run `cargo test --workspace` to verify nothing is broken.

### Phase 88 — Fuzz testing the IR parser

- [ ] Add fuzz testing for the IR parser using `cargo-fuzz`. Create `crates/weaveffi-ir/fuzz/fuzz_targets/parse_yaml.rs`, `parse_json.rs`, `parse_toml.rs`, `parse_type_ref.rs`. Each fuzz target reads arbitrary input bytes and calls the parser, asserting no panic. Run `cargo fuzz build` in CI to ensure the fuzz harness compiles (do not run actual fuzzing in CI; that's for OSS-Fuzz integration later). Run `cargo test --workspace` to verify nothing is broken.

- [ ] Add a property-based test using `proptest` for `TypeRef` round-tripping. Generate arbitrary `TypeRef` values, serialize to YAML/JSON/TOML, parse back, assert equality. Add to `crates/weaveffi-ir/tests/proptest_typeref.rs`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 89 — Symbol mangling collision check

- [ ] Add a validator that detects symbol-name collisions in the C ABI. Two functions in different modules that both end up with `weaveffi_X_Y` after the prefix is applied must collide. The current naming convention `weaveffi_{module}_{function}` gives namespacing, but cross-module struct accessors / builders / iter_next functions can collide if name choices are unlucky. Implement a `collect_c_symbols(api: &Api, c_prefix: &str) -> HashMap<String, Vec<&str>>` function in `crates/weaveffi-core/src/validate.rs` that enumerates every symbol the C generator would emit and detects duplicates. Add a `ValidationError::CSymbolCollision { symbol: String, locations: Vec<String> }`. Add tests `c_symbol_collision_detected`, `unique_symbols_pass`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 90 — Memory leak detection in tests

- [ ] Run all generator tests under leak detection. Add `LeakSanitizer` for Linux: in `.github/workflows/ci.yml` add a sanitizer matrix entry running `cargo test --target x86_64-unknown-linux-gnu` with `RUSTFLAGS="-Z sanitizer=address" CARGO_PROFILE_TEST_LTO=false` (requires nightly toolchain, run as separate job). Note: this is a soft check; tests that legitimately leak in error paths should be marked `#[ignore = "checking with leak sanitizer manually"]`. Run `cargo test --workspace` to verify nothing is broken.

- [ ] For the C example, run under Valgrind in CI. Add a CI step on Ubuntu: `apt-get install -y valgrind` then `valgrind --error-exitcode=1 --leak-check=full ./c_example`. Fix any leaks found in the calculator sample's Rust implementation. Run `cargo test --workspace` to verify nothing is broken.

### Phase 91 — IDL schema for IDE support

- [ ] Generate a JSON Schema for `weaveffi.yml` files. Create `crates/weaveffi-cli/src/main.rs` `SchemaJson` subcommand that prints a JSON Schema describing the API IDL format. Use `schemars = "0.8"` (workspace dep) on the IR types via `#[derive(JsonSchema)]`. Publish the schema at `https://docs.weaveffi.com/schema/v1/api.schema.json`. Document in the README how to add `# yaml-language-server: $schema=https://docs.weaveffi.com/schema/v1/api.schema.json` to IDL files for VS Code completions. Add a CI step that publishes the schema to GitHub Pages alongside the docs. Add a test `schema_json_is_valid_json_schema`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 92 — VS Code extension scaffold

- [ ] Add a minimal VS Code extension at `vscode-extension/`. `package.json` with `contributes.languages` for `weaveffi-yaml` (file pattern `**/weaveffi*.yml`), `contributes.jsonValidation` referencing the JSON Schema from Phase 91, `contributes.commands` for "WeaveFFI: Generate Bindings" and "WeaveFFI: Validate". The extension shells out to the installed `weaveffi` CLI. Include a brief `README.md` and a `screenshot.png` placeholder. Add to a publish workflow (manual trigger, not on every release). Run `cargo test --workspace` to verify nothing is broken.

### Phase 93 — Hardening: thread-safety markers

- [ ] Document and (where possible) enforce thread-safety in the C ABI. In `crates/weaveffi-abi/src/lib.rs`, add doc comments to every public function declaring whether it is `Send` / `Sync` / "main thread only" / "not thread-safe". Update the C generator to include `#ifdef __APPLE__ #include <TargetConditionals.h> #endif` and add `__attribute__((no_sanitize("thread")))` annotations where appropriate. In each generator's wrapper, document the thread-safety assumption (most are "thread-compatible" — single-threaded use is safe, multi-threaded requires synchronisation by the caller). Update `docs/src/guides/memory.md` with a thread-safety section. Run `cargo test --workspace` to verify nothing is broken.

### Phase 94 — Hardening: signed integer overflow guards

- [ ] Audit Rust ABI implementations for signed integer overflow. Sample crates (calculator, contacts, inventory) that perform arithmetic on `i32`/`i64` should use `checked_add`/`checked_mul` or `wrapping_*` deliberately, not silent overflow. Update `samples/calculator/src/lib.rs` `weaveffi_calculator_add` to use `checked_add` and return `WFFI_ERR_OVERFLOW` on overflow. Update `multiply` similarly. Add tests `add_overflow_sets_error`, `mul_overflow_sets_error`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 95 — Final pass: README rewrite

- [ ] Rewrite `README.md` for 1.0. Top: clear elevator pitch (one sentence: "WeaveFFI generates idiomatic FFI bindings for 11 languages from a single API definition file."). Then: badges (CI, license, crates.io for each ecosystem now that they're all published, codecov, security score). Quickstart with a five-line YAML and the resulting Swift/Python output side by side. Feature matrix linking to the per-target tutorials. Install section with all four install methods (cargo, brew, scoop, deb/rpm). Comparison table (vs UniFFI, cbindgen, cxx, swift-bridge — be respectful and accurate about each tradeoff). Sponsors / contributors section. Keep the README under 400 lines; link out to the docs site for everything else. Run `cargo test --workspace` to verify nothing is broken.

### Phase 96 — Final pass: roadmap update

- [ ] Update `docs/src/roadmap.md` for 1.0. Move all PRD-v1 / v2 / v3 / v4 completed items to a "Completed in 1.0" section. Add a new "Post-1.0" section listing speculative future work: Zig generator, OCaml generator, Erlang/Elixir generator, gRPC bridge, language server protocol implementation, IDE plugin (Jetbrains, Sublime, Neovim), web playground (try-it-in-browser via wasm-pack), GraphQL schema bridge. Be clear that these are speculative, not committed. Run `cargo test --workspace` to verify nothing is broken.

### Phase 97 — Final pass: CHANGELOG generation

- [ ] Verify the semantic-release flow generates a clean 1.0.0 CHANGELOG entry. Do a dry-run release on a fork: tag a prerelease, run the release workflow, inspect the generated CHANGELOG section for completeness. Adjust `.releaserc.json` if any commit type is missing or mis-categorised. Do NOT manually edit `CHANGELOG.md` — semantic-release does that. Document the release process in `CONTRIBUTING.md` so future maintainers can run it. Run `cargo test --workspace` to verify nothing is broken.

### Phase 98 — Final pass: clippy strict mode

- [ ] Adopt clippy's stricter lint groups in `crates/weaveffi-core/src/lib.rs` (and equivalents per-crate). Add `#![warn(clippy::pedantic, clippy::cargo, clippy::nursery)]` to each crate's `lib.rs` or `main.rs`. Allow specific lints that produce too much noise (`clippy::module_name_repetitions`, `clippy::missing_errors_doc` if every fn has the same error type). Fix every warning. Update `.github/workflows/ci.yml` clippy step to add the strict groups: `cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic -W clippy::cargo`. Run `cargo test --workspace` to verify nothing is broken.

### Phase 99 — Final pass: cargo machete (unused deps)

- [ ] Install `cargo-machete` and remove unused dependencies. Run `cargo install cargo-machete && cargo machete --with-metadata` and remove any flagged dependencies from each crate's `Cargo.toml`. Add a CI step running `cargo machete` on every push that fails if unused deps are found. Run `cargo test --workspace` to verify nothing is broken.

### Phase 100 — 1.0 release prep: version bump readiness

- [ ] Verify the release pipeline is fully green by triggering a `1.0.0-rc.1` prerelease tag manually. Inspect every published crate on crates.io, every prebuilt binary, the Homebrew formula, the Scoop manifest, the .deb, the .rpm, the MSI, the SBOM, the cosign signatures, the docs site, and the GitHub Release page. File issues for every defect found. Once all defects are fixed, document the "ready for 1.0.0" sign-off in a new `docs/src/release-checklist.md` for future major releases. Add to SUMMARY.md. Run `cargo test --workspace` to verify nothing is broken.

- [ ] Final quality pass. Run `cargo fmt --all` to format everything. Run `cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic -W clippy::cargo -W clippy::nursery` and fix any remaining warnings. Run `cargo test --workspace --all-features --no-fail-fast` and ensure all tests pass on Linux, macOS, and Windows. Run `cargo doc --workspace --no-deps --document-private-items` with `RUSTDOCFLAGS="-D warnings"` and fix any rustdoc warnings. Run `cargo machete` and remove any unused deps. Run `cargo deny check` and fix any policy violations. Run `mdbook build docs && mdbook test docs`. Run `cargo audit` and address any advisories. Verify `cargo package --list -p weaveffi-cli` does not include unexpected files. Verify `.gitignore` covers `.weaveffi-cache`, `target/`, `node_modules/`, `*.dylib`, `*.so`, `*.dll`, `*.aar`, `*.xcframework/`, `dist/`, `*.cdx.json`, `*.sig`, `vscode-extension/node_modules/`. Do NOT manually create or edit `CHANGELOG.md` — it is generated automatically by semantic-release. Do NOT manually bump versions in `Cargo.toml` files — versions are updated automatically by `scripts/update-cargo-versions.sh` during the release process.
