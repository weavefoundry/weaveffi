# Swift iOS App

## Goal

Build a small Rust greeter library, generate Swift bindings with
WeaveFFI, and call them from a SwiftUI iOS app running in the
simulator.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Xcode 15 or later with the iOS SDK installed.
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- iOS Rust targets:

  ```bash
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```

## Step-by-step

### 1. Author the IDL

Save as `greeter.yml`:

```yaml
version: "0.3.0"
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

### 2. Generate bindings

```bash
weaveffi generate greeter.yml -o generated --scaffold
```

You should see, among other targets:

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

### 3. Implement the Rust library

```bash
cargo init --lib mygreeter
```

`mygreeter/Cargo.toml`:

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

`mygreeter/src/lib.rs`:

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
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) { abi::free_string(ptr); }

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) { abi::free_bytes(ptr, len); }

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) { abi::error_clear(err); }
```

Use `scaffold.rs` as the template for the rest of the API
(`weaveffi_greeter_greeting`, the `Greeting` lifecycle, getters, ...).

### 4. Build for iOS targets

```bash
cargo build -p mygreeter --target aarch64-apple-ios --release
cargo build -p mygreeter --target aarch64-apple-ios-sim --release
cargo build -p mygreeter --target x86_64-apple-ios --release
```

Combine the simulator architectures with `lipo` and bundle everything
in an `XCFramework` so Xcode can pick the right slice automatically:

```bash
mkdir -p target/universal-ios-sim/release
lipo -create \
  target/aarch64-apple-ios-sim/release/libmygreeter.a \
  target/x86_64-apple-ios/release/libmygreeter.a \
  -output target/universal-ios-sim/release/libmygreeter.a

xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libmygreeter.a \
  -headers generated/c/ \
  -library target/universal-ios-sim/release/libmygreeter.a \
  -headers generated/c/ \
  -output MyGreeter.xcframework
```

### 5. Wire it into Xcode

1. Create a new iOS App in Xcode (SwiftUI or UIKit).
2. Drag `MyGreeter.xcframework` into the project navigator. Confirm it
   appears under **Build Phases > Link Binary With Libraries**.
3. **File > Add Package Dependencies > Add Local…** and pick
   `generated/swift/`. The package contributes the `CWeaveFFI` and
   `WeaveFFI` targets.
4. **Build Settings > Header Search Paths**: add the path to
   `generated/c/` (e.g. `$(SRCROOT)/../generated/c`).
5. **Build Settings > Library Search Paths**: add the path to the
   matching Rust static library
   (`$(SRCROOT)/../target/aarch64-apple-ios/release` for device
   builds).
6. **Build Phases > Dependencies**: ensure `WeaveFFI` is listed.

### 6. Call from Swift

```swift
import SwiftUI
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

- `Greeter.hello(_:)` — returns `String`.
- `Greeter.greeting(_:_:)` — returns a `Greeting` instance with
  `.message` and `.lang` properties; `deinit` calls the Rust
  destructor automatically.
- `Greeting` — the wrapper class around the opaque Rust pointer.

## Verification

- Select an iOS Simulator target and press **Cmd+R**.
- Tap **Greet** in the running app; the label changes to
  `Hello, Swift!`.
- Re-run on a physical device after building for `aarch64-apple-ios`
  to confirm the device path also works.
- Common error mappings:

  | Symptom                                           | Likely cause                                                                 |
  |---------------------------------------------------|------------------------------------------------------------------------------|
  | `Undefined symbols for architecture arm64`        | Static library not linked or the search path is wrong.                       |
  | `Module 'CWeaveFFI' not found`                    | Header search path does not point at `generated/c/`.                         |
  | `No such module 'WeaveFFI'`                       | Local Swift package not added under **Add Package Dependencies > Add Local…**.|
  | Crash when running on Intel simulator              | Build for `x86_64-apple-ios` and combine with `lipo`.                        |

## Cleanup

```bash
rm -rf generated/ MyGreeter.xcframework
cargo clean -p mygreeter
```

Remove the `MyGreeter.xcframework` reference from the Xcode project
and undo the **Header Search Paths** / **Library Search Paths**
edits.

## Next steps

- See the [Swift generator reference](../generators/swift.md) for the
  full type mapping.
- Read the [Memory Ownership](../guides/memory.md) guide to understand
  struct lifecycle and `deinit` rules.
- Try the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough or [Android](android.md) for a JVM target.
