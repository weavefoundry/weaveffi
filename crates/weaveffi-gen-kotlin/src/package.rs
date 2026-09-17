//! Gradle module scaffolding: `settings.gradle.kts`, `build.gradle.kts`, the
//! CMake build script compiling the JNI shim, and the packaged layout's
//! README and bundled C header.

use std::fmt::Write as _;

use weaveffi_core::cabi;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::PackageContext;
use weaveffi_core::utils::{
    render_abi_prefix_aliases, render_prelude, render_trailer, CommentStyle,
};

use crate::runtime::jni_lib_name;

/// Escape `s` for placement inside a double-quoted Kotlin (Gradle Kotlin DSL)
/// string literal: backslashes, double quotes, and `$` gain a backslash, so a
/// package name or project name containing any of them can't terminate the
/// literal or start a template.
pub(crate) fn kts_quote(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
}

/// Render `settings.gradle.kts`, naming the root project after the resolved
/// package identity.
pub(crate) fn settings_gradle_kts(project_name: &str, input_basename: &str) -> String {
    format!(
        "{}rootProject.name = \"{}\"\n\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        kts_quote(project_name),
        render_trailer(CommentStyle::DoubleSlash, "settings.gradle.kts"),
    )
}

/// The shared body of the Android library module's `build.gradle.kts`: the
/// Kotlin plugin, the JVM `namespace`, the SDK levels the generated code
/// needs (`java.lang.ref.Cleaner` is API 33), and the JNI shim wired through
/// `externalNativeBuild`. `extra` is spliced inside the `android {}` block.
fn gradle_kts_body(namespace: &str, extra: &str) -> String {
    let namespace = kts_quote(namespace);
    format!(
        r#"plugins {{
    id("com.android.library")
    id("org.jetbrains.kotlin.android") version "1.9.22"
}}

android {{
    namespace = "{namespace}"
    compileSdk = 34
    defaultConfig {{
        // java.lang.ref.Cleaner, which backs every wrapper's disposal, is API 33.
        minSdk = 33
        externalNativeBuild {{
            cmake {{
                cppFlags("")
            }}
        }}
    }}
    externalNativeBuild {{
        cmake {{
            path = file("src/main/cpp/CMakeLists.txt")
        }}
    }}
{extra}}}

dependencies {{
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.0")
}}
"#
    )
}

/// Render `build.gradle.kts` for the generated (source-only) module.
pub(crate) fn build_gradle_kts(namespace: &str, input_basename: &str) -> String {
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        gradle_kts_body(namespace, ""),
        render_trailer(CommentStyle::DoubleSlash, "build.gradle.kts"),
    )
}

/// Render `build.gradle.kts` for the packaged module: the same Android
/// library, plus the bundled `jniLibs/` directory as a JNI source set.
pub(crate) fn packaged_build_gradle_kts(namespace: &str, input_basename: &str) -> String {
    let extra = "    sourceSets {\n        getByName(\"main\") {\n            jniLibs.srcDirs(\"src/main/jniLibs\")\n        }\n    }\n";
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        gradle_kts_body(namespace, extra),
        render_trailer(CommentStyle::DoubleSlash, "build.gradle.kts"),
    )
}

/// Render `CMakeLists.txt` for the generated layout: one shared library
/// `lib{prefix}.so` built from the JNI shim, with the sibling `c/` output
/// (the generated `{prefix}.h`) on the include path. Linking the producer
/// library is left to the consumer's build.
pub(crate) fn cmake_lists(c_prefix: &str, input_basename: &str) -> String {
    let body = format!(
        "cmake_minimum_required(VERSION 3.22)\n\
         project({c_prefix}_jni C)\n\
         add_library({c_prefix} SHARED weaveffi_jni.c)\n\
         target_include_directories({c_prefix} PRIVATE ../../../../c)\n"
    );
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::Hash, input_basename),
        render_trailer(CommentStyle::Hash, "CMakeLists.txt"),
    )
}

/// Render `CMakeLists.txt` for the packaged layout: builds `lib{lib}_jni` from
/// the shim against the bundled `include/{prefix}.h`, and links the bundled
/// producer library for the current Android ABI (from `jniLibs/<abi>/`) or,
/// on a desktop JVM build, from `src/main/resources/natives/<platform id>/`
/// when the caller passes `WEAVEFFI_PLATFORM_ID`.
pub(crate) fn packaged_cmake_lists(lib: &str, c_prefix: &str, input_basename: &str) -> String {
    let jni = jni_lib_name(lib);
    let body = format!(
        r#"cmake_minimum_required(VERSION 3.22)
project({jni} C)

# The shim compiles against the bundled include/{c_prefix}.h.
add_library({jni} SHARED weaveffi_jni.c)
target_include_directories({jni} PRIVATE include)

if(ANDROID)
    # The AAR bundles the producer library per ABI under jniLibs/.
    add_library({lib} SHARED IMPORTED)
    set_target_properties({lib} PROPERTIES IMPORTED_LOCATION
        ${{CMAKE_CURRENT_SOURCE_DIR}}/../jniLibs/${{ANDROID_ABI}}/lib{lib}.so)
    target_link_libraries({jni} PRIVATE {lib})
else()
    # Desktop JVM: build against a JDK and the bundled desktop binary.
    find_package(JNI REQUIRED)
    target_include_directories({jni} PRIVATE ${{JNI_INCLUDE_DIRS}})
    if(NOT DEFINED WEAVEFFI_PLATFORM_ID)
        message(FATAL_ERROR "set -DWEAVEFFI_PLATFORM_ID=<darwin-arm64|linux-x64|...> to pick the bundled producer library")
    endif()
    set(WEAVEFFI_NATIVES ${{CMAKE_CURRENT_SOURCE_DIR}}/../resources/natives/${{WEAVEFFI_PLATFORM_ID}})
    target_link_directories({jni} PRIVATE ${{WEAVEFFI_NATIVES}})
    target_link_libraries({jni} PRIVATE {lib})
    set_target_properties({jni} PROPERTIES
        LIBRARY_OUTPUT_DIRECTORY ${{WEAVEFFI_NATIVES}}
        RUNTIME_OUTPUT_DIRECTORY ${{WEAVEFFI_NATIVES}})
endif()
"#
    );
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::Hash, input_basename),
        render_trailer(CommentStyle::Hash, "CMakeLists.txt"),
    )
}

/// Render the `{prefix}.h` the packaged shim compiles against, so the module
/// is self-contained without the sibling `c/` output: the same include
/// guard, runtime declarations, prefix aliases, and per-module declarations
/// the C generator emits, composed from [`weaveffi_core::cabi`].
pub(crate) fn packaged_header(
    model: &BindingModel,
    input_basename: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix.as_str();
    let guard = format!("{}_H", prefix.to_uppercase());
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let _ = write!(out, "#ifndef {guard}\n#define {guard}\n\n");
    out.push_str("#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n\n");
    cabi::render_visibility_macros(&mut out, prefix);
    out.push_str(&render_abi_prefix_aliases(prefix));
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    cabi::render_runtime_decls(&mut out, prefix);
    cabi::render_decls(&mut out, &model.modules, prefix, true);
    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    let _ = write!(out, "#endif // {guard}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Render the packaged module's README: the layout, how Android consumers
/// depend on the module, how a desktop JVM build compiles the shim against
/// the bundled binary, and the list of bundled platforms.
pub(crate) fn packaged_readme(
    project_name: &str,
    package: &str,
    lib: &str,
    c_prefix: &str,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let jni = jni_lib_name(lib);
    let mut android: Vec<String> = Vec::new();
    let mut desktop: Vec<String> = Vec::new();
    for nb in &ctx.binaries.binaries {
        let filename = ctx.binaries.bundled_filename(nb.platform);
        if let Some(abi) = nb.platform.android_abi() {
            android.push(format!("- `src/main/jniLibs/{abi}/{filename}`"));
        } else if nb.platform.is_desktop() {
            desktop.push(format!(
                "- `src/main/resources/natives/{}/{filename}`",
                nb.platform.id()
            ));
        }
    }
    let android_list = if android.is_empty() {
        "- (none bundled)".to_string()
    } else {
        android.join("\n")
    };
    let desktop_list = if desktop.is_empty() {
        "- (none bundled)".to_string()
    } else {
        desktop.join("\n")
    };
    format!(
        r#"{prelude}# {project_name} (Kotlin)

An Android library module (Gradle, Kotlin DSL) exposing the `{package}` API
over a JNI shim. The Kotlin sources live under `src/main/kotlin/`, the shim
under `src/main/cpp/` with the C header it compiles against in
`src/main/cpp/include/{c_prefix}.h`, and the prebuilt producer libraries under
`src/main/jniLibs/<abi>/` (Android) and `src/main/resources/natives/<platform>/`
(desktop JVMs).

## Android

Add the module to your project (`include(":{project_name}")` in
`settings.gradle.kts`, or publish it as an AAR) and depend on it. The NDK's
CMake builds `lib{jni}.so` against the bundled `lib{lib}.so` for each ABI, and
the Kotlin wrapper loads it with `System.loadLibrary("{jni}")`. `minSdk` is
33 because the wrappers' disposal backstop is `java.lang.ref.Cleaner`.

## Desktop JVM

Build the shim once per platform against a JDK, next to the bundled producer
binary, so the wrapper can extract both from the classpath at runtime:

```bash
cmake -S src/main/cpp -B build -DWEAVEFFI_PLATFORM_ID=darwin-arm64
cmake --build build
```

The wrapper picks `natives/<os>-<arch>/` from `os.name` and `os.arch`, loads
the producer library first and the shim second, and falls back to
`System.loadLibrary("{jni}")` (the `java.library.path`) when the resources
are absent.

## Bundled Android ABIs

{android_list}

## Bundled desktop platforms

{desktop_list}

{trailer}"#
    )
}
