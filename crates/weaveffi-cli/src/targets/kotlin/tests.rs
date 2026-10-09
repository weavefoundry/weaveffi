//! Unit tests of what's specific to the Kotlin target: identity-driven names
//! and layout, configuration overrides (package, flavor, SDK levels), name
//! escaping and module objects, JNI name mangling, and packaging. The
//! `kitchen_sink` snapshot pins the rest of the generated surface.

use camino::Utf8PathBuf;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::package::{FileContent, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::kotlin::{KotlinConfig, KotlinFlavor, KotlinGenerator};
use crate::targets::Target;

const FIXTURE: &str = r#"
version: "0.12.0"
modules:
  - name: bus
    enums:
      - name: Level
        variants:
          - { name: Low, value: 0 }
          - { name: High, value: 1 }
    structs:
      - name: Event
        fields:
          - { name: id, type: i64 }
          - { name: source, type: Store }
    callback_interfaces:
      - name: Subscriber
        methods:
          - name: on_event
            params:
              - { name: event, type: Event }
              - { name: note, type: string }
            return: Level
          - name: label
            return: string
          - name: pick
            params:
              - { name: home, type: Store }
            return: "Store?"
          - name: fetch
            params:
              - { name: key, type: string }
            return: bytes
            throws: BusError
          - name: weight
            params:
              - { name: count, type: u32 }
            return: u64
    errors:
      - name: BusError
        codes:
          - { name: Closed, code: 1, message: "Bus is closed" }
          - name: Missing
            code: 2
            message: "missing"
            fields:
              - { name: key, type: string }
              - { name: tries, type: u16 }
    interfaces:
      - name: Store
        constructors:
          - name: new
            params:
              - { name: name, type: string }
        methods:
          - name: get
            params:
              - { name: key, type: string }
            return: "string?"
            throws: BusError
          - name: larger
            params:
              - { name: other, type: "Store?" }
            return: "Store?"
          - name: keys
            params: []
            return: "iter<string>"
          - name: counts
            params: []
            return: "iter<i32>"
          - name: subscribe
            params:
              - { name: sub, type: Subscriber }
          - name: watch
            params:
              - { name: sub, type: "Subscriber?" }
          - name: sizes
            params: []
            return: "iter<u32>"
          - name: drain
            params: []
            return: i64
            async: true
            cancellable: true
            throws: BusError
    functions:
      - name: get
        params:
          - { name: id, type: i64 }
        return: Event
      - name: scale
        params:
          - { name: by, type: u8 }
          - { name: totals, type: "{u64:u16}" }
        return: "[u32]"
    modules:
      - name: stats
        structs:
          - name: Stats
            fields:
              - { name: total, type: i64 }
        functions:
          - name: summarize
            params:
              - { name: store, type: Store }
            return: Stats
  - name: units
    enums:
      - name: Unit
        variants:
          - { name: Px, value: 0 }
    functions:
      - name: echo
        params:
          - { name: unit, type: Unit }
        return: Unit
"#;

fn api() -> Model {
    let api = weaveffi_model::parse::parse_api_str(FIXTURE, "yaml").expect("fixture parses");
    weaveffi_model::validate::validate(&api, &Identity::named("event-bus"), None)
        .expect("fixture validates")
}

/// Render with `config` and return `(path, contents)` pairs, paths with `/`.
fn render(config: &KotlinConfig) -> Vec<(String, String)> {
    let model = api();
    KotlinGenerator::from(config.clone())
        .render(&model)
        .into_iter()
        .map(|f| (f.path.as_str().replace('\\', "/"), f.contents))
        .collect()
}

fn file<'a>(files: &'a [(String, String)], suffix: &str) -> &'a str {
    files
        .iter()
        .find(|(p, _)| p.ends_with(suffix))
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| {
            let paths: Vec<&String> = files.iter().map(|f| &f.0).collect();
            panic!("{suffix} missing from {paths:?}")
        })
}

#[test]
fn layout_follows_the_identity() {
    let files = render(&KotlinConfig::default());
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "settings.gradle.kts",
            "build.gradle.kts",
            "consumer-rules.pro",
            "src/main/cpp/CMakeLists.txt",
            "src/main/cpp/event_bus.h",
            "src/main/cpp/event_bus_jni.c",
            "src/main/kotlin/event_bus/Runtime.kt",
            "src/main/kotlin/event_bus/Buffers.kt",
            "src/main/kotlin/event_bus/Async.kt",
            "src/main/kotlin/event_bus/Codecs.kt",
            "src/main/kotlin/event_bus/JniBridge.kt",
            "src/main/kotlin/event_bus/Bus.kt",
            "src/main/kotlin/event_bus/Units.kt",
        ]
    );
    let runtime = file(&files, "Runtime.kt");
    assert!(runtime.contains("package event_bus\n"));
    assert!(runtime.contains("private const val LIBRARY = \"event_bus\""));
    assert!(runtime.contains("private const val JNI_LIBRARY = \"event_bus_jni\""));
    assert!(runtime.contains("System.getenv(\"EVENT_BUS_LIBRARY\")"));
    let settings = file(&files, "settings.gradle.kts");
    assert!(settings.contains("rootProject.name = \"event-bus\""));
    assert!(settings.contains("google()"));
    let gradle = file(&files, "build.gradle.kts");
    assert!(gradle.contains("id(\"com.android.library\")"));
    assert!(gradle.contains("namespace = \"event_bus\""));
    assert!(gradle.contains("minSdk = 21"));
    assert!(gradle.contains("`maven-publish`"));
    assert!(gradle.contains("kotlinx-coroutines-core"));
    let cmake = file(&files, "CMakeLists.txt");
    assert!(cmake.contains("add_library(event_bus_jni SHARED event_bus_jni.c)"));
    assert!(cmake.contains("find_library(EVENT_BUS_PRODUCER NAMES event_bus"));
    assert!(cmake.contains("../jniLibs/${ANDROID_ABI}"));
    // The bundled header is the C target's, verbatim.
    for (path, contents) in files.iter().filter(|(p, _)| !p.ends_with(".h")) {
        let body: String = contents.lines().skip(3).collect::<Vec<_>>().join("\n");
        assert!(
            !body.contains("WeaveFFI") && !body.contains("weaveffi"),
            "{path} carries WeaveFFI branding"
        );
    }
}

#[test]
fn configuration_overrides_package_and_flavor() {
    let config = KotlinConfig {
        name: Some("com.example.bus".into()),
        flavor: KotlinFlavor::Jvm,
        ..KotlinConfig::default()
    };
    let files = render(&config);
    assert!(files
        .iter()
        .all(|(p, _)| !p.ends_with("consumer-rules.pro")));
    assert!(files
        .iter()
        .any(|(p, _)| p == "src/main/kotlin/com/example/bus/JniBridge.kt"));
    let gradle = file(&files, "build.gradle.kts");
    assert!(gradle.contains("kotlin(\"jvm\")"));
    assert!(gradle.contains("group = \"com.example.bus\""));
    assert!(!gradle.contains("com.android.library"));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("Java_com_example_bus_JniBridge_bus_1Store_1get("));
    assert!(c.contains("FindClass(env, \"com/example/bus/JniBridge\")"));
}

#[test]
fn modules_are_objects_and_shadowing_names_are_escaped() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    // Two modules each declare `get`; the module objects keep them apart,
    // and the `stats` object steps aside for the `Stats` record.
    assert!(bus.contains("object Bus {"));
    assert!(bus.contains("    fun get(id: Long): Event = "));
    assert!(bus.contains("    object StatsModule {"));
    assert!(bus.contains("fun summarize(store: Store): Stats = store.handle.borrow { _h0 ->"));
    // A user type named like a Kotlin builtin gains a trailing underscore.
    let units = file(&files, "Units.kt");
    assert!(units.contains("enum class Unit_(val value: Int) {"));
    assert!(units.contains(
        "fun echo(unit: Unit_): Unit_ = Unit_.fromValue(JniBridge.units_echo(unit.value))"
    ));
    // JNI natives are named after their (unique) C symbols.
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge.contains("@JvmStatic external fun bus_get(id: Long): ByteArray"));
    assert!(bridge.contains("@JvmStatic external fun bus_stats_summarize(store: Long): ByteArray"));
}

#[test]
fn package_bundles_prebuilt_binaries() {
    let model = api();
    let mut binaries = BinarySet::new("event_bus");
    let mut android = NativeBinary::new(
        Platform::AndroidArm64,
        "/prebuilt/android-arm64/libevent_bus.so",
    );
    android.jni_shim = Some(Utf8PathBuf::from(
        "/prebuilt/android-arm64/libevent_bus_jni.so",
    ));
    binaries.insert(android);
    let mut mac = NativeBinary::new(
        Platform::MacosArm64,
        "/prebuilt/darwin-arm64/libevent_bus.dylib",
    );
    mac.jni_shim = Some(Utf8PathBuf::from(
        "/prebuilt/darwin-arm64/libevent_bus_jni.dylib",
    ));
    binaries.insert(mac);
    binaries.insert(NativeBinary::new(
        Platform::Wasm32,
        "/prebuilt/wasm32/event_bus.wasm",
    ));
    let ctx = PackageContext::new(&binaries);
    let artifacts = KotlinGenerator::from(KotlinConfig::default())
        .package(&model, &ctx)
        .expect("kotlin supports packaging");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "kotlin/event-bus");
    let paths: Vec<&str> = artifacts[0].files.iter().map(|f| f.path.as_str()).collect();
    for expected in [
        "README.md",
        "build.gradle.kts",
        "src/main/jniLibs/arm64-v8a/libevent_bus.so",
        "src/main/jniLibs/arm64-v8a/libevent_bus_jni.so",
        "src/main/resources/natives/darwin-arm64/libevent_bus.dylib",
        "src/main/resources/natives/darwin-arm64/libevent_bus_jni.dylib",
    ] {
        assert!(paths.contains(&expected), "{expected} missing: {paths:?}");
    }
    assert!(!paths.iter().any(|p| p.contains("wasm")));
    let text = |name: &str| match &artifacts[0].file(name).expect(name).content {
        FileContent::Text(t) => t.clone(),
        _ => panic!("{name} is text"),
    };
    let readme = text("README.md");
    assert!(readme.contains("- `src/main/jniLibs/arm64-v8a/libevent_bus.so`"));
    assert!(readme.contains("without CMake or the NDK"));
    let gradle = text("build.gradle.kts");
    assert!(
        gradle.contains("include(\"**/libevent_bus_jni.so\")"),
        "{gradle}"
    );
    assert!(gradle.contains("    if (!prebuiltJni) {"), "{gradle}");

    // Without a shim for every Android ABI, none ships: CMake builds them.
    let mut partial = BinarySet::new("event_bus");
    partial.insert(NativeBinary::new(
        Platform::AndroidX64,
        "/prebuilt/android-x64/libevent_bus.so",
    ));
    partial.insert(binaries.get(Platform::AndroidArm64).unwrap().clone());
    let artifacts = KotlinGenerator::from(KotlinConfig::default())
        .package(&model, &PackageContext::new(&partial))
        .unwrap();
    assert!(!artifacts[0]
        .files
        .iter()
        .any(|f| f.path.as_str().ends_with("_jni.so")));
}

#[test]
fn android_sdk_levels_are_configurable() {
    let config = KotlinConfig {
        min_sdk: 24,
        compile_sdk: 34,
        ..KotlinConfig::default()
    };
    let files = render(&config);
    let gradle = file(&files, "build.gradle.kts");
    assert!(gradle.contains("    compileSdk = 34\n"), "{gradle}");
    assert!(gradle.contains("        minSdk = 24\n"), "{gradle}");
}
