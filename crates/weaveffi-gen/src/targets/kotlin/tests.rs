//! Unit tests rendering a small IR through the generator: identity-driven
//! names and layout, configuration overrides, name escaping, the ABI 3 JNI
//! surface (ptr+len strings, load-time checks, cancellation), object
//! lifetime, callbacks, iterators, and packaging.

use camino::{Utf8Path, Utf8PathBuf};
use weaveffi_model::model::BindingModel;
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;

use crate::backend::LanguageBackend;
use crate::package::{FileContent, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::kotlin::{KotlinConfig, KotlinFlavor, KotlinGenerator};

const FIXTURE: &str = r#"
version: "0.10.0"
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
    errors:
      name: BusError
      codes:
        - { name: Closed, code: 1, message: "Bus is closed" }
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
            throws: true
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
          - name: drain
            params: []
            return: i64
            async: true
            cancellable: true
            throws: true
    functions:
      - name: get
        params:
          - { name: id, type: i64 }
        return: Event
    modules:
      - name: stats
        structs:
          - name: Stats
            fields:
              - { name: total, type: i64 }
        functions:
          - name: get
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

fn api() -> ResolvedApi {
    let api = weaveffi_model::parse::parse_api_str(FIXTURE, "yaml").expect("fixture parses");
    weaveffi_model::validate::validate_api(api, None)
        .expect("fixture validates")
        .with_identity(Identity::named("event-bus"))
}

/// Render with `config` and return `(path, contents)` pairs, paths with `/`.
fn render(config: &KotlinConfig) -> Vec<(String, String)> {
    let api = api();
    let model = BindingModel::build(&api);
    KotlinGenerator
        .files(&api, &model, Utf8Path::new("out"), config)
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
            "out/kotlin/settings.gradle.kts",
            "out/kotlin/build.gradle.kts",
            "out/kotlin/consumer-rules.pro",
            "out/kotlin/src/main/cpp/CMakeLists.txt",
            "out/kotlin/src/main/cpp/event_bus.h",
            "out/kotlin/src/main/cpp/event_bus_jni.c",
            "out/kotlin/src/main/kotlin/event_bus/Runtime.kt",
            "out/kotlin/src/main/kotlin/event_bus/Buffers.kt",
            "out/kotlin/src/main/kotlin/event_bus/Async.kt",
            "out/kotlin/src/main/kotlin/event_bus/JniBridge.kt",
            "out/kotlin/src/main/kotlin/event_bus/Bus.kt",
            "out/kotlin/src/main/kotlin/event_bus/Units.kt",
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
        package: Some("com.example.bus".into()),
        flavor: KotlinFlavor::Jvm,
        ..KotlinConfig::default()
    };
    let files = render(&config);
    assert!(files
        .iter()
        .all(|(p, _)| !p.ends_with("consumer-rules.pro")));
    assert!(files
        .iter()
        .any(|(p, _)| p == "out/kotlin/src/main/kotlin/com/example/bus/JniBridge.kt"));
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
    assert!(bus.contains("fun get(store: Store): Stats = store.handle.borrow { _h0 ->"));
    // A user type named like a Kotlin builtin gains a trailing underscore.
    let units = file(&files, "Units.kt");
    assert!(units.contains("enum class Unit_(val value: Int) {"));
    assert!(units.contains(
        "fun echo(unit: Unit_): Unit_ = Unit_.fromValue(JniBridge.units_echo(unit.value))"
    ));
    // JNI natives are named after their (unique) C symbols.
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge.contains("@JvmStatic external fun bus_get(id: Long): ByteArray"));
    assert!(bridge.contains("@JvmStatic external fun bus_stats_get(store: Long): ByteArray"));
}

#[test]
fn strings_cross_as_bytes_with_lengths() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains(
        "fun get(key: String): String? = handle.borrow { _self ->\n        decodeBuffer(JniBridge.bus_Store_get(_self, encodeUtf8(key))) { _r -> _r.readOptional { _r.readString() } }"
    ));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("Jni_bytes p_key_b = Jni_borrow_bytes(env, p_key);"));
    assert!(c.contains(
        "event_bus_bus_Store_get((const event_bus_bus_Store*)(intptr_t)self, p_key_b.ptr, p_key_b.len, &out_len, &err);"
    ));
    assert!(c.contains("return Jni_take_bytes(env, rv, out_len);"));
    assert!(c.contains("event_bus_free_bytes((uint8_t*)ptr, len);"));
    assert!(!c.contains("free_string"));
    assert!(!c.contains("GetStringUTFChars"));
}

#[test]
fn load_checks_the_abi_revision_and_every_root_checksum() {
    let api = api();
    let model = BindingModel::build(&api);
    let files = render(&KotlinConfig::default());
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("if (event_bus_abi_version() != 3u) {"));
    for root in model.roots() {
        let sum = root.checksum.expect("roots carry a checksum");
        assert!(c.contains(&format!(
            "if (event_bus_{}_checksum() != UINT64_C(0x{sum:016x})) {{",
            root.name
        )));
        assert!(c.contains(&format!(
            "module '{}' does not match these bindings",
            root.name
        )));
    }
}

#[test]
fn objects_are_borrowed_for_every_call() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains("class Store private constructor(address: Long) : AutoCloseable {"));
    assert!(bus.contains(
        "internal val handle: NativeHandle = NativeCleaner.register(this, NativeHandle(address, JniBridge::bus_Store_destroy))"
    ));
    assert!(bus.contains(
        "fun larger(other: Store?): Store? = handle.borrow { _self ->\n        other?.handle.borrowOrNull { _h0 ->\n            Store.fromHandleOrNull(JniBridge.bus_Store_larger(_self, _h0))"
    ));
    // Objects inside buffers are cloned on the way in and released again if
    // the encoding fails.
    assert!(bus.contains("_w.writeObject(_v.source.cloneHandle(), JniBridge::bus_Store_destroy)"));
    let runtime = file(&files, "Runtime.kt");
    assert!(runtime.contains("PhantomReference"));
    assert!(!runtime.contains("java.lang.ref.Cleaner"));
}

#[test]
fn cancellable_async_wires_the_token() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains(
        "suspend fun drain(): Long = awaitNative(true, 1, { it as Long }) { _token, _done ->"
    ));
    assert!(bus.contains("JniBridge.bus_Store_drain(_self, _token, _done)"));
    let runtime = file(&files, "Async.kt");
    assert!(runtime.contains("cont.invokeOnCancellation { completion.cancel() }"));
    assert!(runtime.contains("if (code == -5)"));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains(
        "(event_bus_cancel_token*)(intptr_t)cancel_token, Jni_done_bus_Store_drain, context);"
    ));
    assert!(c.contains("CallVoidMethod(env, (jobject)context, Jni_on_long, (jlong)result);"));
}

#[test]
fn callbacks_dispatch_through_cached_static_shims() {
    let files = render(&KotlinConfig::default());
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge.contains(
        "fun bus_Subscriber_on_event(_impl: Subscriber, event: ByteArray, note: ByteArray): Int = _impl.onEvent(decodeBuffer(event) { _r -> unpackEvent(_r) }, decodeUtf8(note)).value"
    ));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains(
        "GetStaticMethodID(env, Jni_bridge, \"bus_Subscriber_on_event\", \"(Levent_bus/Subscriber;[B[B)I\");"
    ));
    assert!(c.contains("pthread_key_create(&Jni_env_key, Jni_detach);"));
    assert!(c.contains("AttachCurrentThreadAsDaemon"));
    assert!(c.contains("event_bus_error_set(out_err, -4,"));
    assert!(c.contains(
        "static const event_bus_bus_Subscriber_vtable Jni_bus_Subscriber_vtable = {Jni_bus_Subscriber_on_event, Jni_release_callback};"
    ));
}

#[test]
fn iterators_stream_through_native_iterator() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains(
        "fun keys(): NativeIterator<String> = handle.borrow { _self ->\n        NativeIterator(JniBridge.bus_Store_keys(_self), JniBridge::bus_Store_KeysIterator_destroy) { _it ->"
    ));
    assert!(bus.contains(
        "if (JniBridge.bus_Store_CountsIterator_next(_it, _slot)) _slot[0] else NativeIterator.DONE"
    ));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("(*env)->SetIntArrayRegion(env, out, 0, 1, &value);"));
}

#[test]
fn package_bundles_prebuilt_binaries() {
    let api = api();
    let model = BindingModel::build(&api);
    let mut binaries = BinarySet::new("event_bus");
    binaries.binaries.push(NativeBinary {
        platform: Platform::AndroidArm64,
        source: Utf8PathBuf::from("/prebuilt/android-arm64/libevent_bus.so"),
    });
    binaries.binaries.push(NativeBinary {
        platform: Platform::MacosArm64,
        source: Utf8PathBuf::from("/prebuilt/darwin-arm64/libevent_bus.dylib"),
    });
    binaries.binaries.push(NativeBinary {
        platform: Platform::Wasm32,
        source: Utf8PathBuf::from("/prebuilt/wasm32/event_bus.wasm"),
    });
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("bus.yml"),
    };
    let files = KotlinGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &KotlinConfig::default(),
        )
        .expect("kotlin supports packaging");
    let paths: Vec<String> = files
        .iter()
        .map(|f| f.path.as_str().replace('\\', "/"))
        .collect();
    for expected in [
        "out/kotlin/README.md",
        "out/kotlin/src/main/jniLibs/arm64-v8a/libevent_bus.so",
        "out/kotlin/src/main/resources/natives/darwin-arm64/libevent_bus.dylib",
    ] {
        assert!(paths.iter().any(|p| p == expected), "{expected} missing");
    }
    assert!(!paths.iter().any(|p| p.contains("wasm")));
    let readme = files
        .iter()
        .find(|f| f.path.as_str().ends_with("README.md"))
        .map(|f| match &f.content {
            FileContent::Text(t) => t.clone(),
            FileContent::Copy(_) => panic!("README is text"),
        })
        .expect("README");
    assert!(readme.contains("- `src/main/jniLibs/arm64-v8a/libevent_bus.so`"));
    assert!(readme.contains("-DEVENT_BUS_LIBRARY_DIR="));
}
