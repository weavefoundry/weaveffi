//! Unit tests of what's specific to the Kotlin target: identity-driven names
//! and layout, configuration overrides (package, flavor, SDK levels), name
//! escaping, the JNA binding of the C ABI, and packaging. The
//! `kitchen_sink` snapshot pins the rest of the generated surface.

use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::package::{FileContent, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::kotlin::{KotlinConfig, KotlinFlavor, KotlinGenerator};
use crate::targets::Target;

const FIXTURE: &str = r#"
version: "0.13.0"
enums:
  - name: Level
    variants:
      - { name: Low, value: 0 }
      - { name: High, value: 1 }
  - name: Unit
    variants:
      - { name: Px, value: 0 }
records:
  - name: Event
    fields:
      - { name: id, type: i64 }
      - { name: source, type: Store }
  - name: Pointer
    fields:
      - { name: at, type: i64 }
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
callbacks:
  - name: Subscriber
    methods:
      - name: on_event
        params:
          - { name: event, type: Event }
          - { name: note, type: string }
        return: Level
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
          - { name: count, type: "u32?" }
        return: bool
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
      - name: keys
        params: []
        return: "iter<string>"
      - name: counts
        params: []
        return: "iter<u32?>"
      - name: subscribe
        params:
          - { name: sub, type: Subscriber }
      - name: watch
        params:
          - { name: sub, type: "Subscriber?" }
        return: bool
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
      - { name: totals, type: "[u64]" }
    return: "[u32]"
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
            "src/main/kotlin/event_bus/Runtime.kt",
            "src/main/kotlin/event_bus/Abi.kt",
            "src/main/kotlin/event_bus/Buffers.kt",
            "src/main/kotlin/event_bus/Async.kt",
            "src/main/kotlin/event_bus/Codecs.kt",
            "src/main/kotlin/event_bus/CLibrary.kt",
            "src/main/kotlin/event_bus/EventBus.kt",
        ]
    );
    let runtime = file(&files, "Runtime.kt");
    assert!(runtime.contains("package event_bus\n"));
    assert!(runtime.contains("private const val LIBRARY = \"event_bus\""));
    assert!(runtime.contains("private const val ABI_VERSION = 6"));
    assert!(runtime.contains("System.getenv(\"EVENT_BUS_LIBRARY\")"));
    assert!(runtime.contains("Native.load(name, CLibrary::class.java, options)"));
    assert!(runtime.contains("function(library, \"event_bus_abi_version\")"));
    let abi = file(&files, "Abi.kt");
    assert!(abi.contains("fun event_bus_run_free(run: Run.ByValue)"));
    assert!(abi.contains("internal fun optU32(value: UInt?): OptU32.ByValue"));
    let settings = file(&files, "settings.gradle.kts");
    assert!(settings.contains("rootProject.name = \"event-bus\""));
    assert!(settings.contains("google()"));
    let gradle = file(&files, "build.gradle.kts");
    assert!(gradle.contains("id(\"com.android.library\")"));
    assert!(gradle.contains("namespace = \"event_bus\""));
    assert!(gradle.contains("minSdk = 21"));
    assert!(gradle.contains("`maven-publish`"));
    assert!(gradle.contains("implementation(\"net.java.dev.jna:jna:5.18.1@aar\")"));
    assert!(gradle.contains("kotlinx-coroutines-core"));
    assert!(!gradle.contains("cmake") && !gradle.contains("externalNativeBuild"));
    let rules = file(&files, "consumer-rules.pro");
    assert!(rules.contains("-keep class event_bus.** { *; }"));
    assert!(rules.contains("-keep class com.sun.jna.* { *; }"));
    for (path, contents) in &files {
        let body: String = contents.lines().skip(3).collect::<Vec<_>>().join("\n");
        assert!(
            !body.contains("WeaveFFI") && !body.contains("weaveffi"),
            "{path} carries WeaveFFI branding"
        );
        assert!(
            !body.contains("JNI") && !body.contains("jni_"),
            "{path} mentions JNI"
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
        .any(|(p, _)| p == "src/main/kotlin/com/example/bus/CLibrary.kt"));
    let gradle = file(&files, "build.gradle.kts");
    assert!(gradle.contains("kotlin(\"jvm\")"));
    assert!(gradle.contains("group = \"com.example.bus\""));
    assert!(gradle.contains("implementation(\"net.java.dev.jna:jna:5.18.1\")"));
    assert!(!gradle.contains("com.android.library") && !gradle.contains("@aar"));
    assert!(file(&files, "EventBus.kt").contains("package com.example.bus\n"));
}

#[test]
fn shadowing_names_are_escaped() {
    let files = render(&KotlinConfig::default());
    let decls = file(&files, "EventBus.kt");
    // The free functions live on an object named after the library.
    assert!(decls.contains("object EventBus {"));
    assert!(decls.contains("    fun get(id: Long): Event = "));
    // A user type named like a Kotlin builtin or an imported JNA type gains
    // a trailing underscore.
    assert!(decls.contains("enum class Unit_(val value: Int) {"));
    assert!(decls.contains("data class Pointer_(val at: Long)"));
    assert!(decls.contains(
        "fun echo(unit: Unit_): Unit_ = Unit_.fromValue(callNative(0) { _err -> Lib.c.event_bus_echo(unit.value, _err) })"
    ));
    // An object steps aside for a type of the same name.
    let model = weaveffi_model::validate::validate(
        &weaveffi_model::parse::parse_api_str(
            "version: \"0.13.0\"\nrecords: [{ name: Store, fields: [{ name: n, type: i32 }] }]\nfunctions: [{ name: f, return: Store }]\n",
            "yaml",
        )
        .unwrap(),
        &Identity::named("store"),
        None,
    )
    .unwrap();
    let files = KotlinGenerator::from(KotlinConfig::default()).render(&model);
    assert!(files
        .iter()
        .any(|f| f.path.as_str().ends_with("StoreLibrary.kt")
            && f.contents.contains("object StoreLibrary {")));
}

#[test]
fn the_jna_interface_mirrors_the_header() {
    let files = render(&KotlinConfig::default());
    let c = file(&files, "CLibrary.kt");
    for decl in [
        "fun event_bus_Store_new(name: Run.ByValue, out_err: NativeError?): Pointer?",
        "fun event_bus_Store_get(self: Pointer?, key: Run.ByValue, out_err: NativeError?): Run.ByValue",
        "fun event_bus_Store_keys(self: Pointer?, out_err: NativeError?): Pointer?",
        "fun event_bus_Store_KeysIterator_next(iter: Pointer?, out_item: Run, out_err: NativeError?): Int",
        "fun event_bus_Store_CountsIterator_next(iter: Pointer?, out_item: OptU32, out_err: NativeError?): Int",
        "fun event_bus_Store_KeysIterator_destroy(iter: Pointer?)",
        "fun event_bus_Store_subscribe(self: Pointer?, sub: CallbackArg, out_err: NativeError?)",
        "fun event_bus_Store_watch(self: Pointer?, sub: CallbackArg, out_err: NativeError?): Byte",
        "fun event_bus_Store_drain(self: Pointer?, cancel_token: Pointer?, callback: Completions.I64Callback, context: Pointer?)",
        "fun event_bus_Store_clone(self: Pointer?): Pointer?",
        "fun event_bus_scale(by: Byte, totals: Run.ByValue, out_err: NativeError?): Run.ByValue",
    ] {
        assert!(c.contains(decl), "missing `{decl}` in:\n{c}");
    }
    // Every declaration's contract row, checked at load.
    assert!(c.contains("ContractRow(\"event_bus_contract_"));
    assert!(c.contains("\"Subscriber.on_event\"),"));
    assert!(c.contains("domain == 2 -> BusException.fromCode(code, message, payload)"));
    // The vtable: a static table of static callbacks, each slot's C shape.
    assert!(c.contains("internal object SubscriberVtable {"));
    assert!(c.contains(
        "@Structure.FieldOrder(\"size\", \"flags\", \"free\", \"slot0\", \"slot1\", \"slot2\", \"slot3\")"
    ));
    assert!(c.contains(
        "fun invoke(ctx: Pointer?, event: Run.ByValue, note: Run.ByValue, out_err: Pointer?): Int"
    ));
    assert!(c.contains("fun invoke(ctx: Pointer?, count: OptU32.ByValue, out_err: Pointer?): Byte"));
    // An object argument is adopted before anything can fail; a typed
    // failure reports its domain's code.
    assert!(c.contains("val home_ = Store.fromHandle(home)"));
    assert!(c.contains("reportFailure(_err, _e, _e as? BusException, -1)"));
    assert!(c.contains("reportFailure(_err, _e, null, -4)"));
    assert!(c.contains("allocRun(Callbacks.get<Subscriber>(_ctx).fetch(key.string()))"));
    assert!(c.contains("val i64Done = I64Callback { context, err, result ->"));
    let decls = file(&files, "EventBus.kt");
    assert!(decls.contains("suspend fun drain(): Long = awaitNative(true, 2, { _raw -> _raw as Long }) { _token, _context ->"));
    assert!(decls.contains("fun counts(): NativeSequence<UInt?> {"));
    assert!(decls.contains("            NativeSequence<UInt?>(\n"));
    assert!(
        decls.contains("Lib.c.event_bus_Store_watch(_self, SubscriberVtable.argOrNone(sub), _err)")
    );
    assert!(decls.contains("val totals_ = runOf(totals.toLongArrayBits())"));
}

#[test]
fn package_bundles_only_the_producer_libraries() {
    let model = api();
    let mut binaries = BinarySet::new("event_bus");
    for (p, path) in [
        (
            Platform::AndroidArm64,
            "/prebuilt/android-arm64/libevent_bus.so",
        ),
        (
            Platform::MacosArm64,
            "/prebuilt/darwin-arm64/libevent_bus.dylib",
        ),
        (Platform::LinuxX64, "/prebuilt/linux-x64/libevent_bus.so"),
        (Platform::WindowsX64, "/prebuilt/windows-x64/event_bus.dll"),
        (Platform::Wasm32, "/prebuilt/wasm32/event_bus.wasm"),
    ] {
        binaries.insert(NativeBinary::new(p, path));
    }
    let ctx = PackageContext::new(&binaries);
    let artifacts = KotlinGenerator::from(KotlinConfig::default())
        .package(&model, &ctx)
        .expect("kotlin supports packaging");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "kotlin/event-bus");
    let mut natives: Vec<&str> = artifacts[0]
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.contains("jniLibs") || p.contains("resources"))
        .collect();
    natives.sort_unstable();
    assert_eq!(
        natives,
        [
            "src/main/jniLibs/arm64-v8a/libevent_bus.so",
            "src/main/resources/darwin-aarch64/libevent_bus.dylib",
            "src/main/resources/linux-x86-64/libevent_bus.so",
            "src/main/resources/win32-x86-64/event_bus.dll",
        ]
    );
    let paths: Vec<&str> = artifacts[0].files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"build.gradle.kts") && paths.contains(&"README.md"));
    assert!(!paths
        .iter()
        .any(|p| p.contains("wasm") || p.contains("cpp")));
    let FileContent::Text(readme) = &artifacts[0].file("README.md").expect("README").content else {
        panic!("README.md is text");
    };
    assert!(readme.contains("- `src/main/jniLibs/arm64-v8a/libevent_bus.so`"));
    assert!(readme.contains("- `src/main/resources/darwin-aarch64/libevent_bus.dylib`"));
    assert!(readme.contains("There's no native glue to build."));

    // Nothing to bundle: no artifact.
    let mut wasm = BinarySet::new("event_bus");
    wasm.insert(NativeBinary::new(
        Platform::Wasm32,
        "/prebuilt/wasm32/event_bus.wasm",
    ));
    let artifacts = KotlinGenerator::from(KotlinConfig::default())
        .package(&model, &PackageContext::new(&wasm))
        .unwrap();
    assert!(artifacts.is_empty());
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
