//! Build scaffolding: `settings.gradle.kts`, `build.gradle.kts` for the
//! Android library or plain Kotlin/JVM flavor (both with a `maven-publish`
//! publication), the R8 keep rules, the CMake build for the JNI shim, and
//! the packaged layout's README.

use crate::package::PackageContext;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

use crate::targets::kotlin::runtime::jni_library;
use crate::targets::kotlin::{KotlinConfig, KotlinFlavor};

/// The Kotlin Gradle plugin version the build files pin.
const KOTLIN_VERSION: &str = "2.0.21";
/// The Android Gradle plugin version the Android flavor pins.
const AGP_VERSION: &str = "8.7.3";
/// The coroutines runtime async callables suspend through.
const COROUTINES: &str = "org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0";

/// Escape `s` for a double-quoted Kotlin string literal.
pub(crate) fn kts_quote(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
}

/// Wrap `body` in the generated-file header and trailer.
fn wrap(style: CommentStyle, file: &str, body: &str) -> String {
    format!(
        "{}{body}\n{}",
        render_prelude(style),
        render_trailer(style, file)
    )
}

/// `settings.gradle.kts`: plugin and dependency repositories, so the module
/// builds on its own, and the root project named after the package.
pub(crate) fn settings_gradle(id: &Identity, flavor: KotlinFlavor) -> String {
    let google = match flavor {
        KotlinFlavor::Android => "        google()\n",
        KotlinFlavor::Jvm => "",
    };
    let body = format!(
        r#"pluginManagement {{
    repositories {{
{google}        mavenCentral()
        gradlePluginPortal()
    }}
}}

dependencyResolutionManagement {{
    repositories {{
{google}        mavenCentral()
    }}
}}

rootProject.name = "{}"
"#,
        kts_quote(&id.name)
    );
    wrap(CommentStyle::DoubleSlash, "settings.gradle.kts", &body)
}

/// `build.gradle.kts` for the configured flavor.
pub(crate) fn build_gradle(
    id: &Identity,
    package: &str,
    config: &KotlinConfig,
    has_async: bool,
) -> String {
    let (min_sdk, compile_sdk) = (config.min_sdk, config.compile_sdk);
    let group = kts_quote(package);
    let version = kts_quote(&id.version);
    let artifact = kts_quote(&id.name);
    let deps = if has_async {
        format!("dependencies {{\n    implementation(\"{COROUTINES}\")\n}}\n\n")
    } else {
        String::new()
    };
    let body = match config.flavor {
        KotlinFlavor::Android => format!(
            r#"plugins {{
    id("com.android.library") version "{AGP_VERSION}"
    id("org.jetbrains.kotlin.android") version "{KOTLIN_VERSION}"
    `maven-publish`
}}

group = "{group}"
version = "{version}"

// A packaged build ships a prebuilt lib{jni}.so next to the producer
// library in src/main/jniLibs/<abi>/; without one, CMake builds it.
val prebuiltJni = fileTree("src/main/jniLibs") {{ include("**/lib{jni}.so") }}.files.isNotEmpty()

android {{
    namespace = "{group}"
    compileSdk = {compile_sdk}
    defaultConfig {{
        minSdk = {min_sdk}
        consumerProguardFiles("consumer-rules.pro")
    }}
    // Builds lib{jni}.so per ABI, linked against the producer library in
    // src/main/jniLibs/<abi>/ (see src/main/cpp/CMakeLists.txt).
    if (!prebuiltJni) {{
        externalNativeBuild {{
            cmake {{
                path = file("src/main/cpp/CMakeLists.txt")
            }}
        }}
    }}
    compileOptions {{
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }}
    publishing {{
        singleVariant("release") {{
            withSourcesJar()
        }}
    }}
}}

kotlin {{
    compilerOptions {{
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_1_8)
    }}
}}

{deps}publishing {{
    publications {{
        register<MavenPublication>("release") {{
            artifactId = "{artifact}"
            afterEvaluate {{
                from(components["release"])
            }}
        }}
    }}
}}
"#,
            jni = jni_library(&id.library),
        ),
        KotlinFlavor::Jvm => format!(
            r#"plugins {{
    kotlin("jvm") version "{KOTLIN_VERSION}"
    `java-library`
    `maven-publish`
}}

group = "{group}"
version = "{version}"

// The JNI shim (lib{jni}) and the producer library load from the
// classpath's natives/<platform>/ resources (which a packaged build ships
// under src/main/resources/), else from java.library.path, where a shim
// built with CMake from src/main/cpp goes.

{deps}java {{
    withSourcesJar()
}}

publishing {{
    publications {{
        create<MavenPublication>("maven") {{
            artifactId = "{artifact}"
            from(components["java"])
        }}
    }}
}}
"#,
            jni = jni_library(&id.library),
        ),
    };
    wrap(CommentStyle::DoubleSlash, "build.gradle.kts", &body)
}

/// `consumer-rules.pro`: the JNI shim finds classes and methods by name, so
/// R8 must keep the package intact in consuming apps.
pub(crate) fn consumer_rules(package: &str) -> String {
    let body = format!(
        "# The JNI shim looks classes and methods up by name.\n-keep class {package}.** {{ *; }}\n"
    );
    wrap(CommentStyle::Hash, "consumer-rules.pro", &body)
}

/// `src/main/cpp/CMakeLists.txt`: builds `lib{library}_jni` from the shim
/// and the bundled header, linked against the producer library found in
/// `{PREFIX}_LIBRARY_DIR` (on Android, `src/main/jniLibs/<abi>` by default).
pub(crate) fn cmake_lists(id: &Identity) -> String {
    let lib = &id.library;
    let jni = jni_library(lib);
    let upper = id.macro_prefix();
    let dir_var = format!("{upper}_LIBRARY_DIR");
    let body = format!(
        r#"cmake_minimum_required(VERSION 3.18)
project({jni} C)

# The directory holding the producer library (lib{lib}). Android builds
# default to the per-ABI copy under src/main/jniLibs/.
set({dir_var} "" CACHE PATH "Directory holding the {lib} producer library")
if(NOT {dir_var} AND ANDROID)
    set({dir_var} "${{CMAKE_CURRENT_SOURCE_DIR}}/../jniLibs/${{ANDROID_ABI}}")
endif()
if({dir_var})
    find_library({upper}_PRODUCER NAMES {lib} PATHS "${{{dir_var}}}" NO_DEFAULT_PATH NO_CMAKE_FIND_ROOT_PATH REQUIRED)
else()
    set(CMAKE_FIND_FRAMEWORK NEVER)
    find_library({upper}_PRODUCER NAMES {lib} REQUIRED)
endif()

add_library({jni} SHARED {jni}.c)
target_include_directories({jni} PRIVATE "${{CMAKE_CURRENT_SOURCE_DIR}}")
target_link_libraries({jni} PRIVATE "${{{upper}_PRODUCER}}")
if(NOT ANDROID)
    find_package(JNI REQUIRED)
    find_package(Threads REQUIRED)
    target_include_directories({jni} PRIVATE ${{JNI_INCLUDE_DIRS}})
    target_link_libraries({jni} PRIVATE Threads::Threads)
endif()
"#
    );
    wrap(CommentStyle::Hash, "CMakeLists.txt", &body)
}

/// The packaged module's README: the layout, the bundled binaries, and how
/// each runtime loads them.
pub(crate) fn packaged_readme(id: &Identity, package: &str, ctx: &PackageContext) -> String {
    let jni = jni_library(&id.library);
    let mut android = Vec::new();
    let mut desktop = Vec::new();
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
    let list = |v: Vec<String>| {
        if v.is_empty() {
            "- (none bundled)".to_string()
        } else {
            v.join("\n")
        }
    };
    let body = format!(
        r#"# {name} (Kotlin)

Kotlin bindings for `{name}` in the `{package}` package: the Kotlin sources
under `src/main/kotlin/`, the JNI shim's source and the C header it compiles
against under `src/main/cpp/`, and the prebuilt native libraries.

## Android

The producer library and, when prebuilt, the JNI shim `lib{jni}.so` sit in
`src/main/jniLibs/<abi>/`; with the shim present, Gradle packages both into
the AAR without CMake or the NDK. Build and publish the AAR with Gradle
(`gradle assembleRelease`, `gradle publishToMavenLocal`).

## Desktop JVM

The producer library and the prebuilt shim for each desktop platform sit in
`src/main/resources/natives/<platform>/`. At run time the bindings extract
both from the classpath, falling back to `java.library.path`.

## Bundled Android ABIs

{android}

## Bundled desktop platforms

{desktop}
"#,
        name = id.name,
        android = list(android),
        desktop = list(desktop),
    );
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::Xml),
        render_trailer(CommentStyle::Xml, "README.md")
    )
}
