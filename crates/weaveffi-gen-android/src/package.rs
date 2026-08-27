//! Gradle project scaffolding: `settings.gradle`, `build.gradle`, and the
//! CMake build script compiling the JNI shim.

use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

/// Escape `s` for placement inside a single-quoted Groovy (Gradle) string
/// literal: backslashes and single quotes gain a backslash, so a package
/// name or project name containing either can't terminate the literal.
pub(crate) fn gradle_squote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Render `settings.gradle`, naming the root project after the resolved
/// package identity.
pub(crate) fn settings_gradle(project_name: &str, input_basename: &str) -> String {
    format!(
        "{}rootProject.name = '{}'\n\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        gradle_squote(project_name),
        render_trailer(CommentStyle::DoubleSlash, "settings.gradle"),
    )
}

/// Render `build.gradle` for the Android library module, wiring the JNI shim
/// through `externalNativeBuild` and setting the JVM `namespace`.
pub(crate) fn build_gradle(namespace: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let trailer = render_trailer(CommentStyle::DoubleSlash, "build.gradle");
    let namespace = gradle_squote(namespace);
    format!(
        r#"{prelude}plugins {{
    id 'com.android.library'
    id 'org.jetbrains.kotlin.android' version '1.9.22' apply false
}}

android {{
    namespace '{namespace}'
    compileSdk 34
    defaultConfig {{
        minSdk 24
        externalNativeBuild {{
            cmake {{
                cppFlags ""
            }}
        }}
    }}
    externalNativeBuild {{
        cmake {{
            path "src/main/cpp/CMakeLists.txt"
        }}
    }}
}}

{trailer}"#
    )
}

/// The static body of `CMakeLists.txt`: one shared library built from the
/// JNI shim, with the generated C header on the include path.
const CMAKE: &str = r#"cmake_minimum_required(VERSION 3.22)
project(weaveffi)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
"#;

/// Render `CMakeLists.txt` for the JNI shim.
pub(crate) fn cmake_lists(input_basename: &str) -> String {
    format!(
        "{}{CMAKE}\n{}",
        render_prelude(CommentStyle::Hash, input_basename),
        render_trailer(CommentStyle::Hash, "CMakeLists.txt"),
    )
}
