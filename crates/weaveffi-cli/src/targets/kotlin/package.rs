//! Build scaffolding: `settings.gradle.kts`, `build.gradle.kts` for the
//! Android library or plain Kotlin/JVM flavor (both with a `maven-publish`
//! publication and the JNA dependency), the R8 keep rules, and the packaged
//! layout's README.

use crate::package::PackageContext;
use crate::platform::Platform;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

use crate::targets::kotlin::{KotlinConfig, KotlinFlavor};

/// The Kotlin Gradle plugin version the build files pin.
const KOTLIN_VERSION: &str = "2.0.21";
/// The Android Gradle plugin version the Android flavor pins.
const AGP_VERSION: &str = "8.7.3";
/// The coroutines runtime async callables suspend through.
const COROUTINES: &str = "org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0";
/// The JNA release the bindings call the C ABI through.
pub(crate) const JNA_VERSION: &str = "5.18.1";

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

/// The classpath resource directory JNA extracts a desktop platform's
/// library from (its `Platform.RESOURCE_PREFIX`), or `None` off the
/// desktop.
pub(crate) fn jna_resource_prefix(platform: Platform) -> Option<&'static str> {
    Some(match platform {
        Platform::MacosArm64 => "darwin-aarch64",
        Platform::MacosX64 => "darwin-x86-64",
        Platform::LinuxX64 => "linux-x86-64",
        Platform::LinuxArm64 => "linux-aarch64",
        Platform::WindowsX64 => "win32-x86-64",
        _ => return None,
    })
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
    let jna = match config.flavor {
        // The AAR carries JNA's own native dispatch library per ABI.
        KotlinFlavor::Android => format!("net.java.dev.jna:jna:{JNA_VERSION}@aar"),
        KotlinFlavor::Jvm => format!("net.java.dev.jna:jna:{JNA_VERSION}"),
    };
    let coroutines = if has_async {
        format!("\n    implementation(\"{COROUTINES}\")")
    } else {
        String::new()
    };
    let deps = format!("dependencies {{\n    implementation(\"{jna}\"){coroutines}\n}}\n");
    let lib = &id.library;
    let body = match config.flavor {
        KotlinFlavor::Android => format!(
            r#"plugins {{
    id("com.android.library") version "{AGP_VERSION}"
    id("org.jetbrains.kotlin.android") version "{KOTLIN_VERSION}"
    `maven-publish`
}}

group = "{group}"
version = "{version}"

// The bindings call the producer library through JNA. A packaged build
// ships it as src/main/jniLibs/<abi>/lib{lib}.so, which Gradle packages into
// the AAR.
android {{
    namespace = "{group}"
    compileSdk = {compile_sdk}
    defaultConfig {{
        minSdk = {min_sdk}
        consumerProguardFiles("consumer-rules.pro")
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

{deps}
publishing {{
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
        ),
        KotlinFlavor::Jvm => format!(
            r#"plugins {{
    kotlin("jvm") version "{KOTLIN_VERSION}"
    `java-library`
    `maven-publish`
}}

group = "{group}"
version = "{version}"

// The bindings call the producer library through JNA, which loads it from
// the path in the {env} environment variable, the system's library path, or
// the classpath's <os>-<arch>/ resources (which a packaged build ships under
// src/main/resources/).

{deps}
java {{
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
            env = id.library_env_var(),
        ),
    };
    wrap(CommentStyle::DoubleSlash, "build.gradle.kts", &body)
}

/// `consumer-rules.pro`: JNA binds the C functions by method name and reads
/// structure fields and callback methods reflectively, so R8 must keep the
/// package and JNA intact in consuming apps.
pub(crate) fn consumer_rules(package: &str) -> String {
    let body = format!(
        "# JNA binds C functions by method name and reads structures reflectively.\n\
         -keep class {package}.** {{ *; }}\n\
         -dontwarn java.awt.*\n\
         -keep class com.sun.jna.* {{ *; }}\n\
         -keepclassmembers class * extends com.sun.jna.* {{ public *; }}\n"
    );
    wrap(CommentStyle::Hash, "consumer-rules.pro", &body)
}

/// The packaged module's README: the layout, the bundled libraries, and how
/// each runtime loads them.
pub(crate) fn packaged_readme(id: &Identity, package: &str, ctx: &PackageContext) -> String {
    let mut android = Vec::new();
    let mut desktop = Vec::new();
    for nb in &ctx.binaries.binaries {
        let filename = ctx.binaries.bundled_filename(nb.platform);
        if let Some(abi) = nb.platform.android_abi() {
            android.push(format!("- `src/main/jniLibs/{abi}/{filename}`"));
        } else if let Some(prefix) = jna_resource_prefix(nb.platform) {
            desktop.push(format!("- `src/main/resources/{prefix}/{filename}`"));
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
under `src/main/kotlin/`, which call the C ABI through JNA
(`net.java.dev.jna:jna:{JNA_VERSION}`), and the prebuilt producer library for each
bundled platform. There's no native glue to build.

## Android

The producer library sits in `src/main/jniLibs/<abi>/`, which Gradle packages
into the AAR; JNA's own native library comes with its AAR. Build and publish
with Gradle (`gradle assembleRelease`, `gradle publishToMavenLocal`).

## Desktop JVM

The producer library for each desktop platform sits in
`src/main/resources/<os>-<arch>/`, where JNA finds it on the classpath and
extracts it at run time. Set `{env}` to load a library from a path instead.

## Bundled Android ABIs

{android}

## Bundled desktop platforms

{desktop}
"#,
        name = id.name,
        env = id.library_env_var(),
        android = list(android),
        desktop = list(desktop),
    );
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::Xml),
        render_trailer(CommentStyle::Xml, "README.md")
    )
}
