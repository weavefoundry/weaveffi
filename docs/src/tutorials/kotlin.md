# Kotlin (Android and desktop JVM)

## Goal

Build a small Rust greeter library with `#[weaveffi::module]`, generate the
Kotlin/JNI bindings with WeaveFFI, package them as a Gradle library module
with the cross-compiled cdylib bundled per Android ABI, and call them from an
Android Studio app running on an emulator or a physical device. The same
module also runs on a desktop JVM, which is how you'll smoke-test it before
opening Android Studio.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Android Studio with the NDK installed (via SDK Manager).
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- The Android Rust targets WeaveFFI packages (`android-arm64` and
  `android-x64`; there's no 32-bit platform in the matrix):

  ```bash
  rustup target add aarch64-linux-android x86_64-linux-android
  ```

- For the desktop smoke test: a JDK (17 or newer), CMake 3.22+, and
  `kotlinc`.

## Step-by-step

### 1. Author the IDL

Save as `greeter.yml`:

```yaml
version: "0.9.0"
modules:
  - name: greeter
    errors:
      name: GreeterError
      codes:
        - { name: UnknownLang, code: 1, message: "unknown language" }
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
        throws: true
        params:
          - { name: name, type: string }
          - { name: lang, type: string }
        return: Greeting
```

`hello` can't fail, so it stays non-throwing. `greeting` declares
`throws: true` and reports codes from the module's `GreeterError`
domain when the language is unknown. Check it:

```bash
weaveffi validate greeter.yml
```

You should see `Validation passed` and `1 modules, 2 functions, 1 structs,
0 enums`.

Put a `weaveffi.toml` beside it so the package and the native library get a
stable name (otherwise both default to the IDL's file stem, `greeter`):

```toml
[package]
name = "mygreeter"
version = "0.1.0"
```

### 2. Generate bindings

```bash
weaveffi generate greeter.yml -o generated
```

You should see, among other targets:

```text
generated/
├── c/
│   ├── weaveffi.c
│   └── weaveffi.h
└── kotlin/
    ├── build.gradle.kts
    ├── settings.gradle.kts
    └── src/main/
        ├── kotlin/com/weaveffi/WeaveFFI.kt
        └── cpp/
            ├── weaveffi_jni.c
            └── CMakeLists.txt
```

`WeaveFFI.kt` holds the `WeaveFFI` class (a companion object with one
`@JvmStatic` function per IDL function), the `Greeting` data class, the
`GreeterException` sealed hierarchy, and the value-buffer codec. This bare
output is what you'd read to learn the API; step 6 produces the module you
actually build, with the native libraries bundled in.

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
weaveffi = "0.22"
```

`mygreeter/src/lib.rs` is plain safe Rust. The macro reads the annotated
items and emits the `extern "C"` thunks that the JNI shim calls, so the
module needs no `unsafe` and no hand-written signatures:

```rust
#[weaveffi::module]
pub mod greeter {
    /// Codes the greeter reports from its throwing functions.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum GreeterError {
        /// unknown language
        UnknownLang = 1,
    }

    /// A greeting and the language it was rendered in.
    #[weaveffi::record]
    pub struct Greeting {
        pub message: String,
        pub lang: String,
    }

    /// Greet someone in English.
    #[weaveffi::export]
    pub fn hello(name: String) -> String {
        format!("Hello, {name}!")
    }

    /// Greet someone in the given language, failing on an unknown one.
    #[weaveffi::export]
    pub fn greeting(name: String, lang: String) -> Result<Greeting, GreeterError> {
        let message = match lang.as_str() {
            "en" => format!("Hello, {name}!"),
            "es" => format!("Hola, {name}!"),
            "fr" => format!("Bonjour, {name}!"),
            _ => return Err(GreeterError::UnknownLang),
        };
        Ok(Greeting { message, lang })
    }
}

weaveffi::export_runtime!();
```

`weaveffi extract mygreeter/src/lib.rs` produces the same IDL as
`greeter.yml` (plus the doc comments), so the two stay in step; you can
generate from either. The [Producer Macro](../guides/producer-macro.md)
guide covers every attribute.

### 4. Configure the NDK toolchain

```bash
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/$(ls $HOME/Library/Android/sdk/ndk | sort -V | tail -1)"
export PATH="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"
```

Replace `darwin-x86_64` with `linux-x86_64` on Linux. Add the matching
`linker = ...` entries to `mygreeter/.cargo/config.toml`. The generated
module sets `minSdk = 33` (its wrappers use `java.lang.ref.Cleaner`), so
pick the API 33 clang wrappers:

```toml
[target.aarch64-linux-android]
linker = "aarch64-linux-android33-clang"

[target.x86_64-linux-android]
linker = "x86_64-linux-android33-clang"
```

### 5. Cross-compile for every ABI

```bash
cd mygreeter
cargo build --target aarch64-linux-android --release
cargo build --target x86_64-linux-android --release
```

You should now have:

```text
target/aarch64-linux-android/release/libmygreeter.so
target/x86_64-linux-android/release/libmygreeter.so
```

### 6. Package the Gradle module

`weaveffi package` assembles the Kotlin module with the cdylib bundled under
`src/main/jniLibs/<abi>/` and a `CMakeLists.txt` that links the JNI shim
against it per ABI. From inside `mygreeter/`, let the CLI run the cross
builds itself:

```bash
weaveffi package ../greeter.yml --build mygreeter \
  --platforms android-arm64,android-x64 -t kotlin -o ../dist
```

It prints one `Building mygreeter for Android arm64 (aarch64-linux-android)...`
line per platform, then:

```text
Packaging 'mygreeter' for platforms: android-arm64, android-x64
  kotlin: 7 file(s), 2 bundled binary(ies)
Packaged 1 target(s) into ../dist
```

If you'd rather build in CI and package afterwards, lay the binaries out as
`prebuilt/<platform id>/libmygreeter.so` and pass `--binaries prebuilt`
instead of `--build`; the [Packaging](../guides/packaging.md) guide has the
full recipe. Either way you get:

```text
dist/kotlin/
├── README.md
├── build.gradle.kts
├── settings.gradle.kts
└── src/main/
    ├── kotlin/com/weaveffi/WeaveFFI.kt
    ├── cpp/
    │   ├── CMakeLists.txt
    │   ├── include/weaveffi.h
    │   └── weaveffi_jni.c
    └── jniLibs/
        ├── arm64-v8a/libmygreeter.so
        └── x86_64/libmygreeter.so
```

### 7. Optional: smoke-test on the desktop JVM

The same module runs on your workstation, which is the quickest way to prove
the Rust and the bindings agree before involving an emulator. Add the host
platform to the package (`darwin-arm64`, `darwin-x64`, or `linux-x64`), then
build the shim against your JDK and run a `main`:

```bash
weaveffi package ../greeter.yml --build mygreeter \
  --platforms android-arm64,android-x64,darwin-arm64 -t kotlin -o ../dist
cd ../dist/kotlin
cmake -S src/main/cpp -B build -DWEAVEFFI_PLATFORM_ID=darwin-arm64
cmake --build build
```

CMake writes `libmygreeter_jni.dylib` next to the bundled producer library
under `src/main/resources/natives/darwin-arm64/`; at runtime the wrapper
extracts both from the classpath. Save this as `Main.kt`:

```kotlin
import com.weaveffi.WeaveFFI
import com.weaveffi.GreeterException

fun main() {
    println(WeaveFFI.hello("Android"))
    val g = WeaveFFI.greeting("Ada", "es")
    println("${g.message} (${g.lang})")
    try {
        WeaveFFI.greeting("Ada", "tlh")
    } catch (e: GreeterException.UnknownLang) {
        println("greeting failed: ${e.message} (code ${e.code})")
    }
}
```

Compile it together with the generated wrapper and run with the resources
directory on the classpath:

```bash
kotlinc src/main/kotlin/com/weaveffi/WeaveFFI.kt Main.kt -include-runtime -d app.jar
java -cp app.jar:src/main/resources MainKt
```

Expected output:

```text
Hello, Android!
Hola, Ada! (es)
greeting failed: unknown language (code 1)
```

### 8. Wire it into Android Studio

1. Create a new Android project (Empty Activity, Kotlin, `minSdk` 33+;
   the generated module won't go lower).
2. Include the packaged module in the root `settings.gradle.kts`:

   ```kotlin
   include(":mygreeter")
   project(":mygreeter").projectDir = file("../dist/kotlin")
   ```

3. Add it as a dependency in your app's `build.gradle.kts`:

   ```kotlin
   dependencies {
       implementation(project(":mygreeter"))
   }
   ```

The module's `build.gradle.kts` applies `com.android.library`, points
`externalNativeBuild` at `src/main/cpp/CMakeLists.txt`, and depends on
`kotlinx-coroutines-core`. The NDK's CMake builds `libmygreeter_jni.so`
for each ABI against the bundled `libmygreeter.so`, so there's nothing to
copy into your app's own `jniLibs/`.

### 9. Call from Kotlin

```kotlin
import com.weaveffi.WeaveFFI
import com.weaveffi.GreeterException

class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        findViewById<TextView>(R.id.textView).text = WeaveFFI.hello("Android")

        try {
            val g = WeaveFFI.greeting("Hi", "en")
            println("${g.message} (${g.lang})")
        } catch (e: GreeterException) {
            println("greeting failed: ${e.message}")
        }
    }
}
```

The `WeaveFFI` companion object loads `libmygreeter_jni` on first use
(`System.loadLibrary("mygreeter_jni")` on Android) and exposes:

- `WeaveFFI.hello(name: String): String`: non-throwing.
- `WeaveFFI.greeting(name: String, lang: String): Greeting`: declared
  `throws` in the IDL, so the JNI shim raises `GreeterException.UnknownLang`
  (a sealed class extending `WeaveFFIException`, one nested class per error
  code) on failure. Catch the nested class, the sealed domain, or the
  generic base.

`Greeting` is a plain Kotlin data class decoded from the value buffer the
C ABI returns; there's nothing to close.

## Verification

- Sync Gradle in Android Studio.
- Pick an emulator or a connected device and press **Run** (Shift+F10).
- The text view should display `Hello, Android!` and Logcat should
  show `Hi (en)` from the `Greeting` block.
- Common error mappings:

  | Symptom                                            | Likely cause                                                                |
  |----------------------------------------------------|-----------------------------------------------------------------------------|
  | `UnsatisfiedLinkError: dlopen failed`              | The ABI you're running on isn't in `jniLibs/`; add its platform to `--platforms` and repackage. |
  | `WeaveFFIException` from JNI                       | A WeaveFFI error was raised; inspect the code and message.                    |
  | Linker errors during `cargo build`                 | `ANDROID_NDK_HOME` is not set or the NDK toolchain is missing from `PATH`.    |
  | `No implementation found for native method`        | JNI symbol names do not match the Kotlin package; re-run `weaveffi package`. |
  | `minSdkVersion 33 cannot be smaller than version` | Your app's `minSdk` is below 33; raise it to match the module.               |

## Cleanup

```bash
rm -rf generated/ dist/
cargo clean -p mygreeter
```

Drop the `include(":mygreeter")` lines from `settings.gradle.kts` and
remove the dependency from your app module if you do not want to keep the
packaged bindings around.

## Next steps

- See the [Kotlin generator reference](../generators/kotlin.md) for
  the full type mapping and JNI conventions.
- Read [Error Handling](../guides/errors.md): JNI shims convert C
  errors to `WeaveFFIException` (or a typed domain exception)
  automatically.
- Read [Packaging](../guides/packaging.md) for the full platform matrix
  and a CI recipe that builds every platform before packaging.
- Try the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough or [Swift iOS](swift.md) for a sibling
  mobile target.
