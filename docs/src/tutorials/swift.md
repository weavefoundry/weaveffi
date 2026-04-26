# Tutorial: Swift iOS App

This tutorial walks through building a Rust library, generating Swift
bindings with WeaveFFI, and integrating everything into an Xcode iOS
project.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel)
- Xcode 15+ with iOS SDK (and Xcode command-line tools for `xcodebuild` + `lipo`)
- WeaveFFI CLI installed (`cargo install weaveffi-cli`)
- Apple Rust targets:

```bash
rustup target add \
  aarch64-apple-ios \
  aarch64-apple-ios-sim \
  aarch64-apple-darwin \
  x86_64-apple-darwin
```

## 1) Define your API

Create a file called `greeter.yml`:

```yaml
version: "0.1.0"
modules:
  - name: greeter
    structs:
      - name: Greeting
        fields:
          - { name: message, type: string }
          - { name: lang, type: string }
    functions:
      - name: hello
        params:
          - { name: name, type: string }
        return: string
      - name: greeting
        params:
          - { name: name, type: string }
          - { name: lang, type: string }
        return: Greeting
```

## 2) Generate bindings

```bash
weaveffi generate greeter.yml -o generated --scaffold
```

This produces:

```text
generated/
├── c/
│   └── weaveffi.h
├── swift/
│   ├── Package.swift
│   └── Sources/
│       ├── CWeaveFFI/
│       │   └── module.modulemap
│       └── WeaveFFI/
│           └── WeaveFFI.swift
└── scaffold.rs
```

## 3) Create the Rust library

Create a new Cargo project for the native library:

```bash
cargo init --lib mygreeter
```

**mygreeter/Cargo.toml:**

```toml
[package]
name = "mygreeter"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["staticlib", "cdylib"]

[dependencies]
weaveffi-abi = { version = "0.1" }
```

Use `staticlib` for iOS — Xcode links static libraries into the app
bundle. `cdylib` is included for desktop testing.

**mygreeter/src/lib.rs:**

```rust
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_greeter_hello(
    name_ptr: *const c_char,
    _name_len: usize,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    abi::error_set_ok(out_err);
    let name = unsafe { CStr::from_ptr(name_ptr) }.to_str().unwrap_or("world");
    let msg = format!("Hello, {name}!");
    CString::new(msg).unwrap().into_raw() as *const c_char
}

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr);
}

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
    abi::free_bytes(ptr, len);
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err);
}
```

Fill in the remaining functions (`weaveffi_greeter_greeting`,
`weaveffi_greeter_Greeting_destroy`, getters, etc.) using the generated
`scaffold.rs` as a guide.

## 4) Build the `.xcframework`

The quickest path from a Rust cdylib to a SwiftPM-ready binary is
`weaveffi build --xcframework`. It cross-compiles the Rust crate for
every Apple slice the generated SwiftPM package expects, merges the two
macOS slices into a universal binary with `lipo`, and bundles the
result with `xcodebuild -create-xcframework`:

```bash
weaveffi build greeter.yml -o generated --xcframework
```

The command:

1. Re-runs `weaveffi generate` (ensuring both the C header and SwiftPM
   scaffold exist under `generated/`).
2. Builds the cdylib for `aarch64-apple-ios`, `aarch64-apple-ios-sim`,
   `aarch64-apple-darwin`, and `x86_64-apple-darwin`.
3. Runs `lipo -create` on the two macOS dylibs to produce a universal
   `libmygreeter.dylib` under `target/universal-apple-darwin/release/`.
4. Writes the final bundle to
   `generated/swift/Frameworks/CWeaveFFI.xcframework`, the exact path
   referenced by the `binaryTarget` in the generated `Package.swift`.

`--xcframework` is macOS-only and requires Xcode + Xcode command-line
tools (`xcodebuild`, `lipo`). The C header search path used for each
slice is `generated/c/`.

> **Custom module names.** When `[generators.c].prefix` (or
> `[generators.swift].module_name`) is set, the output filename
> follows suit — e.g. a C prefix of `mylib` produces
> `generated/swift/Frameworks/CMylib.xcframework`.

### Manual fallback

If you'd rather drive the tools yourself, the equivalent commands are:

```bash
cargo build -p mygreeter --target aarch64-apple-ios --release
cargo build -p mygreeter --target aarch64-apple-ios-sim --release
cargo build -p mygreeter --target aarch64-apple-darwin --release
cargo build -p mygreeter --target x86_64-apple-darwin --release

mkdir -p target/universal-apple-darwin/release
lipo -create \
  target/aarch64-apple-darwin/release/libmygreeter.dylib \
  target/x86_64-apple-darwin/release/libmygreeter.dylib \
  -output target/universal-apple-darwin/release/libmygreeter.dylib

xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libmygreeter.dylib \
  -headers generated/c/ \
  -library target/aarch64-apple-ios-sim/release/libmygreeter.dylib \
  -headers generated/c/ \
  -library target/universal-apple-darwin/release/libmygreeter.dylib \
  -headers generated/c/ \
  -output generated/swift/Frameworks/CWeaveFFI.xcframework
```

## 5) Set up the Xcode project

1. **Create a new iOS App** in Xcode (SwiftUI or UIKit).

2. **Add the generated Swift package.** In Xcode, go to
   **File > Add Package Dependencies > Add Local…** and select
   `generated/swift/`. SwiftPM picks up the `CWeaveFFI.xcframework`
   bundle from `generated/swift/Frameworks/` via the `binaryTarget` in
   `Package.swift`, then builds the `CWeaveFFI` (C module map) and
   `WeaveFFI` (Swift wrapper) targets on top of it.

3. **Add a bridging dependency.** In your app target's
   **Build Phases > Dependencies**, ensure `WeaveFFI` is listed.

## 6) Call from Swift

```swift
import WeaveFFI

struct ContentView: View {
    @State private var greeting = ""

    var body: some View {
        VStack {
            Text(greeting)
            Button("Greet") {
                do {
                    greeting = try Greeter.hello("Swift")
                } catch {
                    greeting = "Error: \(error)"
                }
            }
        }
        .padding()
    }
}
```

The generated `WeaveFFI` module exposes:

- `Greeter.hello(_:)` — returns a `String`
- `Greeter.greeting(_:_:)` — returns a `Greeting` object with `.message`
  and `.lang` properties
- `Greeting` — a class wrapping the opaque Rust pointer, with automatic
  cleanup on `deinit`

## 7) Build and run

Select an iOS Simulator or device target in Xcode and press **Cmd+R**.
The app should display "Hello, Swift!" when you tap the button. Because
the `.xcframework` ships every relevant slice, the same project runs on
macOS, an Apple Silicon simulator, and a real iPhone without any
per-target configuration.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `Undefined symbols for architecture arm64` | Rebuild with `weaveffi build --xcframework` and confirm the bundle exists at `generated/swift/Frameworks/CWeaveFFI.xcframework`. |
| `Module 'CWeaveFFI' not found` | Ensure SwiftPM picked up the local package at `generated/swift/` (File > Add Package Dependencies > Add Local…). |
| `No such module 'WeaveFFI'` | Add the `generated/swift/` local package to your Xcode project. |
| `xcodebuild: error: Both ... represent two equivalent library definitions` | You ran `xcodebuild -create-xcframework` manually with both macOS slices; let `weaveffi build --xcframework` handle the `lipo` step, or merge them yourself before passing a single macOS library. |
| `error: toolchain ... does not support target aarch64-apple-ios` | Run the `rustup target add` command from the Prerequisites section. |

## Next steps

- See the [Swift generator reference](../generators/swift.md) for type
  mapping details.
- Read the [Memory Ownership](../guides/memory.md) guide to understand
  struct lifecycle management.
- Explore the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough.
