# WeaveFFI Cleanup Plan

Comprehensive plan to get the repo into a solid, honest foundation before building
new features. Organized into phases that should be implemented in order.

---

## Phase 1: Restructure Crates

The current structure has `weaveffi-core` doing too much (validation, orchestration,
ABI runtime, templates for 4 languages, WASM generation) while most generator crates
are hollow 20-line shells that just call `core::templates::*`. Nothing is published
yet, so this is the right time to fix the architecture.

### 1.1 Create `weaveffi-abi` crate

Extract `crates/weaveffi-core/src/abi.rs` into a new `crates/weaveffi-abi/` crate.

This is the C ABI runtime: `weaveffi_error`, `weaveffi_handle_t`, error helpers,
string/buffer allocation and freeing. It's a fundamentally different concern from
code generation, and user libraries (like `calculator`) should be able to depend
on it without pulling in the entire codegen engine.

**New crate:** `crates/weaveffi-abi/`
- Move `abi.rs` contents to `src/lib.rs`
- Minimal dependencies (just `std`)
- `samples/calculator` depends on `weaveffi-abi` instead of `weaveffi-core`

### 1.2 Move templates into generator crates

Currently `crates/weaveffi-core/src/templates.rs` contains C, Swift, Node, and WASM
templates in one 320-line file. Each generator crate just calls into it. This creates
fake modularity — the crate boundary exists but the code doesn't respect it.

Move each set of template functions into the generator that uses them:

- **C functions** (`render_c_header`, `render_c_convenience_c`, `c_type_for_param`,
  `c_ret_type_for`, `c_symbol_name`, `c_params_sig`, `render_module_header`)
  → `crates/weaveffi-gen-c/src/lib.rs`

- **Swift functions** (`render_swift_wrapper`, `swift_type_for`,
  `swift_call_args_for_params`, `swift_prep_params`, `swift_return_postprocess`,
  `to_camel`)
  → `crates/weaveffi-gen-swift/src/lib.rs`

- **Node functions** (`render_node_dts`, `node_ts_type_for`)
  → `crates/weaveffi-gen-node/src/lib.rs`

- **WASM functions** (`render_wasm_readme`, `render_wasm_js_stub`)
  → new `crates/weaveffi-gen-wasm/src/lib.rs` (see 1.3)

After this, `crates/weaveffi-core/src/templates.rs` is deleted. Each generator is
self-contained.

**Shared utilities** that multiple generators need (like `c_symbol_name`, which
Swift and Android generators also use to call C functions) should stay in core
as a small `utils` module.

### 1.3 Create `weaveffi-gen-wasm` crate

Move `WasmGenerator` from `crates/weaveffi-core/src/codegen.rs` and
`crates/weaveffi-core/src/wasm.rs` into a new `crates/weaveffi-gen-wasm/` crate.
This makes it consistent with all other generators having their own crate.

### 1.4 Slim down `weaveffi-core`

After the moves, `weaveffi-core` should contain only:

- `codegen.rs` — `Generator` trait and `Orchestrator` (minus `WasmGenerator`)
- `validate.rs` — IR validation logic
- `utils.rs` — shared codegen utilities (`c_symbol_name`, case conversion via `heck`)
- `lib.rs` — re-exports

Remove `abi.rs`, `templates.rs`, and `wasm.rs`.

### 1.5 Move `weaveffi-node-addon` into `samples/`

`crates/weaveffi-node-addon/` is hardcoded to the calculator sample — symbol names,
types, and library paths are all calculator-specific. It's not a reusable library.

**Move** `crates/weaveffi-node-addon/` → `samples/node-addon/`

Update its dependency on `weaveffi-core` (for the error struct) to depend on
`weaveffi-abi` instead.

### 1.6 Update workspace configuration

Update root `Cargo.toml`:
- Add `weaveffi-abi` and `weaveffi-gen-wasm` to workspace members
- Move `weaveffi-node-addon` from `crates/` to `samples/` in workspace members
- Update workspace dependencies as needed

Update `crates/weaveffi-cli/src/main.rs`:
- Add `use weaveffi_gen_wasm::WasmGenerator`
- Remove `use weaveffi_core::codegen::WasmGenerator` (no longer there)

### Resulting structure

```
crates/
  weaveffi-ir/          # IR model + parsing (unchanged)
  weaveffi-abi/         # NEW — C ABI runtime (error, handle, memory helpers)
  weaveffi-core/        # Slimmed — Generator trait, Orchestrator, validation, utils
  weaveffi-gen-c/       # Self-contained — C header + shim generation
  weaveffi-gen-swift/   # Self-contained — SwiftPM + Swift wrapper generation
  weaveffi-gen-android/ # Self-contained (already was) — JNI + Kotlin generation
  weaveffi-gen-node/    # Self-contained — Node.js TypeScript types generation
  weaveffi-gen-wasm/    # NEW — WASM stub generation (moved from core)
  weaveffi-cli/         # CLI binary (unchanged)
samples/
  calculator/           # Sample Rust lib (depends on weaveffi-abi, not core)
  node-addon/           # MOVED from crates/ — calculator-specific N-API addon
```

---

## Phase 2: Fix Bugs in Generated Code

These are correctness issues — the generated output won't compile or will behave
incorrectly. After Phase 1, each fix lands in the generator crate that owns the
template.

### 2.1 Swift `check()` reads error code after clearing it

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

The generated Swift code calls `weaveffi_error_clear(&err)` before reading
`err.code`, so every thrown error reports code `0`.

**Fix:** Capture the code before clearing:

```swift
let code = err.code
let message = err.message.flatMap { String(cString: $0) } ?? ""
weaveffi_error_clear(&err)
throw WeaveFFIError.error(code: code, message: message)
```

### 2.2 Swift string interpolation broken in error description

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

The generated `WeaveFFIError.description` produces `"(\(code)) \ (message)"` —
the `\ (message)` has an errant space that breaks Swift string interpolation.

**Fix:** Change to produce `"(\(code)) \(message)"`.

### 2.3 Swift parameter syntax uses space instead of colon

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

`format!("{} {}", p.name, swift_type_for(&p.ty))` produces `name Type` instead of
`name: Type`. Invalid Swift function signatures.

**Fix:** Change format to `"{}: {}"`.

### 2.4 Swift empty-params produces leading comma

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

When a function has no params, the call emits `func( , &err )` — leading comma.

**Fix:** Conditionally omit the comma when `call_args` is empty.

### 2.5 Swift `UnsafePointer<UInt8>(array)` doesn't exist

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

There is no `UnsafePointer` initializer that accepts an `Array`. Generated Swift
for string params won't compile.

**Fix:** Use `withUnsafeBufferPointer` or restructure so the FFI call happens
inside a safe closure.

### 2.6 Swift `Bytes` return is a placeholder

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

The `Bytes` return path returns `()` instead of `Data`. Generated Swift won't
compile for functions that return bytes.

**Fix:** Implement proper `Data` construction from the returned pointer+length.

### 2.7 Swift pointer escapes `withUnsafeBytes` closure

**File:** `crates/weaveffi-gen-swift/src/lib.rs`

A pointer obtained inside `withUnsafeBytes` is assigned to a variable and used
after the closure returns — undefined behavior in Swift.

**Fix:** Restructure so the FFI call happens inside the closure.

### 2.8 Android JNI uses `auto` (C++ keyword) in generated `.c` file

**File:** `crates/weaveffi-gen-android/src/lib.rs`

`auto rv = ...` is C++, not C. The generated `weaveffi_jni.c` won't compile with
a C compiler (which is the default for Android NDK toolchains).

**Fix:** Emit the explicit C type (`int32_t`, `uint32_t`, `int64_t`, `double`,
`weaveffi_handle_t`) based on the return type.

### 2.9 Android JNI release code is unreachable (after `return`)

**File:** `crates/weaveffi-gen-android/src/lib.rs`

`ReleaseStringUTFChars` / `ReleaseByteArrayElements` are emitted after the
`return` statement. They never execute — JNI resources leak on every call.

**Fix:** Move release code before the return, or use a `goto cleanup` pattern.

### 2.10 Android JNI no return after `ThrowNew`

**File:** `crates/weaveffi-gen-android/src/lib.rs`

After `ThrowNew`, execution falls through and uses potentially uninitialized `rv`.
JNI requires returning immediately after setting a pending exception.

**Fix:** Add `return <default>;` inside the error block.

### 2.11 Android `U32` maps to signed `Int`/`jint`

**File:** `crates/weaveffi-gen-android/src/lib.rs`

Values above `INT_MAX` silently become negative. JNI has no unsigned types.

**Fix:** Map `U32` to `Long`/`jlong` to preserve the full range, or document the
limitation.

### 2.12 WASM README has broken markdown code fence

**File:** `crates/weaveffi-gen-wasm/src/lib.rs`

Closing fence is ` `` ` (two backticks) instead of ` ``` ` (three).

**Fix:** Change `"``"` to `` "```" ``.

### 2.13 `to_camel` panics on edge-case input

**File:** `crates/weaveffi-core/src/utils.rs` (or wherever it lands)

`first[..1]` panics if any segment from `split('_')` is empty. Happens with
leading/trailing/consecutive underscores.

**Fix:** Replace with `heck::ToUpperCamelCase`.

### 2.14 Node `Handle` mapped to `number` in TypeScript

**File:** `crates/weaveffi-gen-node/src/lib.rs`

`Handle` is `u64`, but JS `number` loses precision above `2^53`.

**Fix:** Map to `bigint` in the `.d.ts` generation.

### 2.15 TOML parse errors report `(0, 0)` for location

**File:** `crates/weaveffi-ir/src/parse.rs`

TOML parse errors hardcode `line: 0, column: 0` instead of extracting the
actual error span. Users get no location info for TOML syntax errors.

**Fix:** Extract span from the `toml` crate error (0.8+ exposes `.span()`)
and map to line/column, or at minimum include the error's `Display` output
which contains the location.

---

## Phase 3: Fix Repo-Level Blockers

Issues that would block publishing or give users broken instructions.

### 3.1 Add LICENSE files

`Cargo.toml` declares `license = "MIT OR Apache-2.0"` but no license files exist.
This is a crates.io publishing blocker.

**Fix:** Add `LICENSE-MIT` and `LICENSE-APACHE` at the repo root.

### 3.2 Fix CLI binary name

The binary is `weaveffi-cli` (from the crate name), but all docs, examples, and
CLI output say `weaveffi`. Users will get "command not found".

**Fix:** Add `[[bin]] name = "weaveffi"` to `crates/weaveffi-cli/Cargo.toml`.
Update CI workflow to use `weaveffi` instead of `weaveffi-cli`.

### 3.3 Fix Swift module name mismatch

The generator creates module `WeaveFFI` in the modulemap, but
`examples/swift` imports `CWeaveFFI` and docs reference `Sources/CWeaveFFI`.
The example won't compile against generated output.

**Fix:** Make the generator, example, and docs all agree on the module name.
Pick one (`CWeaveFFI` for the C module map, `WeaveFFI` for the Swift wrapper)
and align everything.

### 3.4 Handle missing file extension in CLI

If the input file has no extension, the error message says
`"unsupported input format:  (expected ...)"` with a blank format.

**Fix:** Check for empty extension first:
`bail!("input file has no extension (expected yml|yaml|json|toml)")`.

### 3.5 Update Node.js version in CI

Node 18 reached EOL in April 2025. The CI and example docs reference it.

**Fix:** Update to Node 22 (current LTS) in `.github/workflows/ci.yml` and
`examples/node/README.md`.

### 3.6 Add `rust-toolchain.toml`

No toolchain file exists. CI uses `dtolnay/rust-toolchain@stable` but local
builds have no pinned version, so contributors can get different behavior.

**Fix:** Add `rust-toolchain.toml` at the repo root:

```toml
[toolchain]
channel = "stable"
```

---

## Phase 4: Remove Unused Dependencies

Every crate has phantom dependencies that inflate compile times. Remove them all
in one pass.

### 4.1 Workspace-level unused deps

Remove from `[workspace.dependencies]` in root `Cargo.toml`:

- `semver` — never used (IR `version` is a plain `String`)
- `indoc` — never imported anywhere
- `rayon` — never imported anywhere
- `fs_err` — never imported anywhere (could be used, but isn't currently)
- `tera` — never used (all codegen is string concatenation)
- `walkdir` — never imported anywhere
- `convert_case` — never imported anywhere

### 4.2 `weaveffi-ir` unused deps

Remove from `crates/weaveffi-ir/Cargo.toml`:

- `semver`

### 4.3 `weaveffi-core` unused deps

After Phase 1 moves, core will be much slimmer. Remove any deps that are no
longer needed (likely everything except `anyhow`, `camino`, `weaveffi-ir`,
and `heck`).

### 4.4 All generator crates unused deps

After Phase 1, each generator owns its own templates. Clean up deps in each:

- Remove `tera`, `convert_case` from any that still have them
- Each generator should only depend on what it actually imports

### 4.5 `weaveffi-cli` unused deps

Remove from `crates/weaveffi-cli/Cargo.toml`:

- `tracing` (only `tracing-subscriber` is used; `tracing` macros are never called)

### 4.6 `node-addon` unused deps (now in `samples/`)

Remove from `samples/node-addon/Cargo.toml`:

- `libc` (code uses `std::os::raw::c_char` instead)
- `calculator` (loaded dynamically via `libloading`, not linked statically)

Replace `once_cell` with `std::sync::OnceLock` (stable since Rust 1.70).

---

## Phase 5: Remove Dead Code

### 5.1 Remove `render_node_index_ts` and related ffi-napi code

**File:** Previously in `crates/weaveffi-core/src/templates.rs`

`render_node_index_ts`, `ffi_napi_type_for`, and related functions generate an
ffi-napi TypeScript loader that is never called by any generator. The Node
generator uses N-API addons, not ffi-napi. Do not carry this dead code into the
gen-node crate during Phase 1.

### 5.2 Remove `validate_type_ref` no-op

**File:** `crates/weaveffi-core/src/validate.rs`

Empty function body, called from two places but does nothing.

### 5.3 Remove empty `[workspace.metadata.weaveffi]` section

**File:** `Cargo.toml` (root)

Contains only a placeholder comment. Remove.

### 5.4 Remove redundant `[lib] path = "src/lib.rs"` sections

**Files:** All library crate `Cargo.toml` files.

`src/lib.rs` is the Cargo default. The explicit path adds nothing.

---

## Phase 6: Fix Safety and Correctness Issues

### 6.1 Remove `Clone + Copy` from `weaveffi_error`

**File:** `crates/weaveffi-abi/src/lib.rs` (after Phase 1 move)

The struct holds `*const c_char` allocated via `CString::into_raw`. If copied and
both copies are cleared, the message is double-freed.

**Fix:** Remove `Copy` and `Clone` from the derive. If `Clone` is needed for FFI
ergonomics, implement it manually with documentation.

### 6.2 Fix `c_ptr_to_str` unbounded lifetime

**File:** `crates/weaveffi-abi/src/lib.rs`

The lifetime `'a` is unconstrained — callers can assign `'static` to the returned
`&str`. Potential use-after-free.

**Fix:** Return an owned `String` instead, or tie the lifetime to a reference param.

### 6.3 Add identifier format validation

**File:** `crates/weaveffi-core/src/validate.rs`

Names like `"123"`, `""`, or `"has spaces"` pass validation but produce invalid
identifiers in every target language.

**Fix:** Validate that names match `[a-zA-Z_][a-zA-Z0-9_]*`.

### 6.4 Activate workspace lints

**File:** Root `Cargo.toml` and all member `Cargo.toml` files.

`unsafe_code = "deny"` is declared at workspace level but no crate opts in.

**Fix:** Add `[lints] workspace = true` to every member crate. Add
`#![allow(unsafe_code)]` in crates that legitimately need it (`weaveffi-abi`,
`node-addon`, `calculator`).

---

## Phase 7: Standardize Patterns

### 7.1 Fix `_api` parameter naming

**Files:** `crates/weaveffi-gen-swift/src/lib.rs`,
`crates/weaveffi-gen-android/src/lib.rs`

Both name the parameter `_api` (convention for "unused") but actually use it.

**Fix:** Rename to `api`.

### 7.2 Inherit workspace package fields

**Files:** `crates/weaveffi-cli/Cargo.toml`, `samples/node-addon/Cargo.toml`,
`samples/calculator/Cargo.toml`

These specify `edition = "2021"` directly instead of inheriting from workspace.

**Fix:** Use `edition.workspace = true` (and `license.workspace = true`, etc.)
where applicable.

### 7.3 Standardize path types

**File:** `crates/weaveffi-cli/src/main.rs`

Mixes `std::path::Path` and `camino::Utf8Path` in the same function.

**Fix:** Use `Utf8Path` consistently.

### 7.4 Standardize error-discard style

**File:** `crates/weaveffi-gen-android/src/lib.rs`

Mixes `.ok()` and `let _ =` for discarding `writeln!` results.

**Fix:** Use `let _ =` consistently (more idiomatic for intentional discards).

### 7.5 Fix node-addon platform-specific path

**File:** `samples/node-addon/src/lib.rs`

Hardcoded `libcalculator.dylib` (macOS only).

**Fix:** Use `cfg!` to select the platform-appropriate extension, or use
`libloading`'s platform-aware filename construction.

### 7.6 Deduplicate `WeaveError` struct in node-addon

**File:** `samples/node-addon/src/lib.rs`

Manually redefines `weaveffi_error`. Will silently drift if the ABI changes.

**Fix:** Depend on `weaveffi-abi` and use `weaveffi_error` directly.

### 7.7 Remove unused import in node-addon

**File:** `samples/node-addon/src/lib.rs`

`CString` is imported but never used.

**Fix:** Remove from import.

### 7.8 Kotlin source directory

**File:** `crates/weaveffi-gen-android/src/lib.rs`

Generated `.kt` file is placed in `src/main/java/` instead of `src/main/kotlin/`.

**Fix:** Change to `src/main/kotlin/com/weaveffi`.

### 7.9 Add `PartialEq`, `Eq` derives to IR types

**File:** `crates/weaveffi-ir/src/ir.rs`

All IR types (`Api`, `Module`, `Function`, `Param`, `TypeRef`, `ErrorDomain`,
`ErrorCode`) lack `PartialEq` and `Eq`. This makes testing and assertions
impossible. Also add `Copy` to `TypeRef` (fieldless enum).

---

## Phase 8: Project Tooling and .gitignore

### 8.1 Update .gitignore

Add missing entries:

```
node_modules/
*.node
.DS_Store
```

### 8.2 Add `rustfmt.toml`

No formatting config exists. Needed before Phase 10 adds `cargo fmt --check`
to CI.

**Fix:** Add `rustfmt.toml` at the repo root:

```toml
edition = "2021"
```

### 8.3 Add a `justfile` for developer convenience

The project has a multi-step build: compile Rust, run code generation, build
examples. There's no way to do this in one command. Also, `examples/swift` and
`examples/node` depend on `generated/` which is gitignored — they're broken
after a fresh clone with no obvious way to fix it.

**Fix:** Add a `justfile` (using [just](https://github.com/casey/just)) with
common developer tasks:

```just
# Build the CLI
build:
    cargo build --release

# Generate bindings from the calculator sample
generate:
    cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated

# Build and run examples (generates first)
examples: generate
    # C example
    cc -I generated/c examples/c/main.c -L target/release -lcalculator -o examples/c/main
    # Swift and Node examples require additional platform-specific setup

# Run all tests
test:
    cargo test --workspace

# Check formatting and lints
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Clean generated output
clean:
    rm -rf generated/
    cargo clean
```

This gives contributors `just build`, `just test`, `just generate`, etc. and
makes the broken-examples problem obvious and fixable.

---

## Phase 9: Update Documentation

### 9.1 Replace the roadmap

Replace `docs/src/roadmap.md` with a grounded version that reflects reality:
what works today, what's next, what's further out. Remove aspirational claims
about features that don't exist (async, ffi-napi, published packages).

### 9.2 Fix README.md

- Remove the claim about async support (explicitly rejected by validation)
- Remove overselling of "build/packaging scaffolds"
- Add installation instructions and a quickstart
- Add badges (CI status, license)

### 9.3 Fix CONTRIBUTING.md

- Use proper heading hierarchy (`#` not `###`)
- Add dev environment setup instructions
- Add instructions for running tests

### 9.4 Fix getting-started.md

Currently only covers `mdbook serve`. Should cover how to actually use WeaveFFI.

### 9.5 Fix docs that reference wrong binary name

Update all docs pages that say `weaveffi` to be consistent with whatever we
decide in Phase 3.2 (we're adding `[[bin]] name = "weaveffi"` so the docs will
be correct after that fix).

### 9.6 Fix docs that reference wrong Swift module name

Update `docs/src/generators/swift.md` to match the aligned module naming from
Phase 3.3.

### 9.7 Fix docs that claim ffi-napi

`docs/src/roadmap.md` and `docs/src/generators/node.md` describe the Node
generator as using ffi-napi. It actually uses N-API addons.

### 9.8 Fix macOS-only instructions in docs

`docs/src/generators/node.md`, `docs/src/generators/swift.md`, and
`docs/src/tutorials/calculator.md` show macOS-only commands without Linux
equivalents.

### 9.9 Fix broken link in docs/src/api/README.md

Says "published under the link below" but provides no link.

### 9.10 Remove references to non-existent features

`docs/src/reference/naming.md` lists published package names on crates.io, npm,
PyPI, etc. — none of which exist. Remove or clearly mark as "planned".

---

## Phase 10: CI Improvements

### 10.1 Add clippy and rustfmt checks

Add `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo fmt --all --check` to the CI workflow.

### 10.2 Speed up mdbook install

Replace `cargo install mdbook` (compiles from source, ~3 min) with a pre-built
binary action or cached binary.

### 10.3 Add `Default` impl for `Orchestrator`

Clippy will flag `Orchestrator::new()` without a corresponding `Default`. Add it.

---

## Phase 11: Add Basic Tests

### 11.1 IR round-trip tests

Add tests to `weaveffi-ir` that parse YAML/JSON/TOML and verify the resulting
`Api` struct. Include edge cases (empty modules, missing optional fields).

### 11.2 Validation tests

Add tests to `weaveffi-core` that exercise the validator: duplicate names,
reserved keywords, invalid identifiers, empty modules.

### 11.3 Template snapshot tests

Add snapshot tests for each generator's output. Parse the calculator IDL, run
each generator, and assert the output matches a known-good snapshot. This
prevents regressions in generated code.

### 11.4 CLI integration test

Add a basic integration test that runs the CLI binary against `calculator.yml`
and verifies the generated files exist.

---

## Summary

| Phase | Items | Priority |
|-------|-------|----------|
| 1. Restructure crates | 6 | Critical |
| 2. Fix generated code bugs | 15 | Critical |
| 3. Fix repo-level blockers | 6 | Critical |
| 4. Remove unused deps | 6 | High |
| 5. Remove dead code | 4 | High |
| 6. Fix safety/correctness | 4 | High |
| 7. Standardize patterns | 9 | Medium |
| 8. Project tooling and .gitignore | 3 | Medium |
| 9. Update documentation | 10 | Medium |
| 10. CI improvements | 3 | Low |
| 11. Add basic tests | 4 | Low |
| **Total** | **70** | |

Phase 1 (restructuring) goes first because it changes where all subsequent fixes
land. Phases 2-3 fix things that are actively broken. Phases 4-6 clean up the
codebase. Phases 7-8 standardize and add tooling. Phases 9-11 improve the
developer experience and prevent regressions.
