# Android App

## Goal

Build a small Rust greeter library, generate Kotlin/JNI bindings with
WeaveFFI, and call them from an Android Studio app running on an
emulator or a physical device.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Android Studio with the NDK installed (via SDK Manager).
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- Android Rust targets:

  ```bash
  rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
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
├── android/
│   ├── settings.gradle
│   ├── build.gradle
│   └── src/main/
│       ├── kotlin/com/weaveffi/WeaveFFI.kt
│       └── cpp/
│           ├── weaveffi_jni.c
│           └── CMakeLists.txt
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
crate-type = ["cdylib"]

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

Use `scaffold.rs` for the rest of the API (`weaveffi_greeter_greeting`,
the `Greeting` lifecycle, getters, ...).

### 4. Configure the NDK toolchain

```bash
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/$(ls $HOME/Library/Android/sdk/ndk | sort -V | tail -1)"
export PATH="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"
```

Replace `darwin-x86_64` with `linux-x86_64` on Linux. Add the matching
`linker = ...` entries in `.cargo/config.toml`:

```toml
[target.aarch64-linux-android]
linker = "aarch64-linux-android21-clang"

[target.armv7-linux-androideabi]
linker = "armv7a-linux-androideabi21-clang"

[target.x86_64-linux-android]
linker = "x86_64-linux-android21-clang"
```

### 5. Cross-compile for every ABI

```bash
cargo build -p mygreeter --target aarch64-linux-android --release
cargo build -p mygreeter --target armv7-linux-androideabi --release
cargo build -p mygreeter --target x86_64-linux-android --release
```

You should now have:

```text
target/aarch64-linux-android/release/libmygreeter.so
target/armv7-linux-androideabi/release/libmygreeter.so
target/x86_64-linux-android/release/libmygreeter.so
```

### 6. Wire it into Android Studio

1. Create a new Android project (Empty Activity, Kotlin, `minSdk` 21+).
2. Include the generated module in the root `settings.gradle`:

   ```groovy
   include ':weaveffi'
   project(':weaveffi').projectDir = new File('generated/android')
   ```

3. Add it as a dependency in your app's `build.gradle`:

   ```groovy
   dependencies {
       implementation project(':weaveffi')
   }
   ```

4. Copy the cdylib into `jniLibs` per ABI:

   ```bash
   mkdir -p app/src/main/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}
   cp target/aarch64-linux-android/release/libmygreeter.so \
     app/src/main/jniLibs/arm64-v8a/libmygreeter.so
   cp target/armv7-linux-androideabi/release/libmygreeter.so \
     app/src/main/jniLibs/armeabi-v7a/libmygreeter.so
   cp target/x86_64-linux-android/release/libmygreeter.so \
     app/src/main/jniLibs/x86_64/libmygreeter.so
   ```

5. Confirm the JNI `CMakeLists.txt` in
   `generated/android/src/main/cpp/` includes
   `target_include_directories(... PRIVATE ../../../../c)` so it can
   find `weaveffi.h`.

### 7. Call from Kotlin

```kotlin
import com.weaveffi.WeaveFFI
import com.weaveffi.Greeting

class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        findViewById<TextView>(R.id.textView).text = WeaveFFI.hello("Android")

        Greeting.create("Hi", "en").use { g ->
            println("${g.message} (${g.lang})")
        }
    }
}
```

The generated `WeaveFFI` companion object loads the cdylib lazily and
exposes:

- `WeaveFFI.hello(name: String): String`
- `WeaveFFI.greeting(name: String, lang: String): Long` — opaque
  handle that the `Greeting` wrapper consumes.

`Greeting` implements `Closeable`; either call `.close()` or use
`use { ... }` for deterministic cleanup.

## Verification

- Sync Gradle in Android Studio.
- Pick an emulator or a connected device and press **Run** (Shift+F10).
- The text view should display `Hello, Android!` and Logcat should
  show `Hi (en)` from the `Greeting` block.
- Common error mappings:

  | Symptom                                            | Likely cause                                                                |
  |----------------------------------------------------|-----------------------------------------------------------------------------|
  | `UnsatisfiedLinkError: dlopen failed`              | The cdylib is missing from `jniLibs/` or built for the wrong ABI.            |
  | `RuntimeException` from JNI                        | A WeaveFFI error was raised; inspect the message.                             |
  | Linker errors during `cargo build`                 | `ANDROID_NDK_HOME` is not set or the NDK toolchain is missing from `PATH`.    |
  | `No implementation found for native method`         | JNI symbol names do not match the Kotlin package; re-run `weaveffi generate`. |

## Cleanup

```bash
rm -rf generated/ app/src/main/jniLibs
cargo clean -p mygreeter
```

Drop the `include ':weaveffi'` line from `settings.gradle` and remove
the dependency from your app module if you do not want to keep the
generated bindings around.

## Next steps

- See the [Android generator reference](../generators/android.md) for
  the full type mapping and JNI conventions.
- Read [Error Handling](../guides/errors.md) — JNI shims convert C
  errors to `RuntimeException` automatically.
- Try the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough or [Swift iOS](swift.md) for a sibling
  mobile target.
