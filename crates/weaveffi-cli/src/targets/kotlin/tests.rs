//! Unit tests rendering a small IR through the generator: identity-driven
//! names and layout, configuration overrides, name escaping, the ABI 4 JNI
//! surface (ptr+len strings, the load-time contract check, cancellation),
//! unsigned types, the trap, composite codecs, object lifetime, callbacks
//! (every return family, `throws`, optional callbacks), thread detaching,
//! iterators, and packaging.

use camino::{Utf8Path, Utf8PathBuf};
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::backend::LanguageBackend;
use crate::package::{FileContent, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::kotlin::{KotlinConfig, KotlinFlavor, KotlinGenerator};

const FIXTURE: &str = r#"
version: "0.11.0"
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
            throws: true
          - name: weight
            params:
              - { name: count, type: u32 }
            return: u64
    errors:
      name: BusError
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
            throws: true
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
    KotlinGenerator
        .files(&model, Utf8Path::new("out"), config)
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
            "out/kotlin/src/main/kotlin/event_bus/Codecs.kt",
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
fn strings_cross_as_bytes_with_lengths() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains(
        "fun get(key: String): String? = handle.borrow { _self ->\n        decodeBuffer(JniBridge.bus_Store_get(_self, encodeUtf8(key))) { _r -> unpackOptionalOfString(_r) }"
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
fn load_checks_the_abi_revision_and_every_root_contract() {
    let model = api();
    let files = render(&KotlinConfig::default());
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("if (event_bus_abi_version() != 4u) {"));
    assert!(!c.contains("checksum"));
    for root in model.roots() {
        let entries = weaveffi_model::contract::entries(&model, root);
        let var = format!("Jni_contract_{}", root.name);
        assert!(c.contains(&format!("static const Jni_contract_entry {var}[] = {{")));
        for e in &entries {
            assert!(c.contains(&format!(
                "{{UINT64_C(0x{:016x}), UINT64_C(0x{:016x}), \"{}\"}},",
                e.id, e.hash, e.path
            )));
        }
        assert!(c.contains(&format!(
            "if (Jni_check_contract(env, event_bus_{}_contract, {var}, sizeof {var} / sizeof {var}[0]) != JNI_OK) {{",
            root.name
        )));
    }
    assert!(c.contains("problem = \"is missing from the library\";"));
    assert!(c.contains("problem = \"changed since these bindings were generated\";"));
}

#[test]
fn unsigned_integers_are_kotlin_unsigned_types() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    // Public signatures use the unsigned types; the JNI natives carry the
    // signed type of the same width, converted bit for bit.
    assert!(bus.contains(
        "fun scale(by: UByte, totals: Map<ULong, UShort>): List<UInt> = decodeBuffer(JniBridge.bus_scale(by.toByte(), encodeBuffer { _w -> packMapOfU64ToU16(_w, totals) })) { _r -> unpackListOfU32(_r) }"
    ));
    assert!(bus.contains(
        "class Missing(val key: String, val tries: UShort, message: String = \"missing\")"
    ));
    assert!(bus.contains("fun weight(count: UInt): ULong"));
    assert!(bus.contains(
        "if (JniBridge.bus_Store_SizesIterator_next(_it, _slot)) _slot[0].toUInt() else NativeIterator.DONE"
    ));
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge
        .contains("@JvmStatic external fun bus_scale(by: Byte, totals: ByteArray): ByteArray"));
    assert!(bridge.contains("_impl.weight(count.toUInt()).toLong()"));
    let buffers = file(&files, "Buffers.kt");
    assert!(buffers.contains("fun writeU32(v: UInt) = writeI32(v.toInt())"));
    assert!(buffers.contains("fun readU64(): ULong = readI64().toULong()"));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("JNICALL Java_event_1bus_JniBridge_bus_1scale(JNIEnv* env, jclass cls, jbyte p_by, jbyteArray p_totals)"));
    assert!(c.contains("event_bus_bus_scale((uint8_t)p_by, "));
    assert!(c.contains("static uint64_t Jni_bus_Subscriber_weight(void* ctx, uint32_t p_count, event_bus_error* out_err)"));
}

#[test]
fn composites_have_one_codec_each() {
    let files = render(&KotlinConfig::default());
    let codecs = file(&files, "Codecs.kt");
    assert!(codecs.contains(
        "internal fun packMapOfU64ToU16(_w: BufferWriter, _v: Map<ULong, UShort>) = _w.writeMap(_v, { _w.writeU64(it) }, { _w.writeU16(it) })"
    ));
    assert!(codecs.contains(
        "internal fun unpackOptionalOfString(_r: BufferReader): String? = _r.readOptional { _r.readString() }"
    ));
    assert_eq!(codecs.matches("internal fun packListOfU32(").count(), 1);
    // Call sites call the codec instead of inlining a loop.
    for (path, contents) in files.iter().filter(|(p, _)| p.ends_with(".kt")) {
        if !path.ends_with("Codecs.kt") && !path.ends_with("Buffers.kt") {
            assert!(
                !contents.contains("writeList(") && !contents.contains("readOptional"),
                "{path}"
            );
        }
    }
}

#[test]
fn a_failed_call_that_cannot_fail_traps() {
    let files = render(&KotlinConfig::default());
    let runtime = file(&files, "Runtime.kt");
    assert!(runtime.contains(
        "class NativeBugException(val code: Int, message: String) :\n    IllegalStateException(\"native call failed with code $code: $message\")"
    ));
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge.contains("1 -> BusException.fromCode(code, text, payload)"));
    assert!(bridge.contains("else -> NativeBugException(code, text)"));
    let c = file(&files, "event_bus_jni.c");
    // `get` throws (domain 1); `larger` can't fail, so its failure traps.
    assert!(c.contains("\"(II[B[B)Ljava/lang/Throwable;\""));
    let larger = &c[c.find("bus_1Store_1larger(").unwrap()..];
    assert!(larger.contains("Jni_throw(env, &err, 0);"));
    let get = &c[c.find("bus_1Store_1get(").unwrap()..];
    assert!(get.contains("Jni_throw(env, &err, 1);"));
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
        "suspend fun drain(): Long = awaitNative(true, 1, { _raw -> _raw as Long }) { _token, _done ->"
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
    assert!(c.contains("Jni_complete_end(env, context, detach);"));
}

#[test]
fn callbacks_dispatch_through_cached_static_shims() {
    let files = render(&KotlinConfig::default());
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge.contains(
        "fun bus_Subscriber_on_event(_impl: Subscriber, _err: Long, event: ByteArray, note: ByteArray): Int = try {\n        _impl.onEvent(decodeBuffer(event) { _r -> unpackEvent(_r) }, decodeUtf8(note)).value\n    } catch (_e: Throwable) {\n        fail(_err, _e, null)\n        0\n    }"
    ));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains(
        "GetStaticMethodID(env, Jni_bridge, \"bus_Subscriber_on_event\", \"(Levent_bus/Subscriber;J[B[B)I\");"
    ));
    assert!(c.contains("AttachCurrentThreadAsDaemon"));
    assert!(c.contains(
        "static const event_bus_bus_Subscriber_vtable Jni_bus_Subscriber_vtable = {sizeof(event_bus_bus_Subscriber_vtable), 0, Jni_release_callback, Jni_bus_Subscriber_on_event, Jni_bus_Subscriber_label, Jni_bus_Subscriber_pick, Jni_bus_Subscriber_fetch, Jni_bus_Subscriber_weight};"
    ));
    // The shim reports failures itself through these two exports.
    assert!(c.contains("Java_event_1bus_JniBridge_error_1set(JNIEnv* env, jclass cls, jlong err, jint code, jbyteArray message)"));
    assert!(
        c.contains("event_bus_error_set_payload((event_bus_error*)(intptr_t)err, p.ptr, p.len);")
    );
    assert!(!c.contains("_dealloc"));
}

#[test]
fn callback_methods_return_every_family() {
    let files = render(&KotlinConfig::default());
    let bridge = file(&files, "JniBridge.kt");
    // A string return is encoded; the trampoline copies it into an alloc'd run.
    assert!(bridge.contains(
        "fun bus_Subscriber_label(_impl: Subscriber, _err: Long): ByteArray? = try {\n        encodeUtf8(_impl.label())"
    ));
    // An object parameter is adopted before anything can fail; an optional
    // object return is a fresh reference the producer adopts.
    assert!(bridge.contains(
        "fun bus_Subscriber_pick(_impl: Subscriber, _err: Long, home: Long): Long {\n        val _home = Store.fromHandle(home)\n        return try {\n            _impl.pick(_home)?.cloneHandle() ?: 0L"
    ));
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains(
        "static void Jni_bus_Subscriber_label(void* ctx, uint8_t** out_ptr, size_t* out_len, event_bus_error* out_err) {"
    ));
    assert!(c.contains("Jni_callback_return_bytes(env, rv, out_ptr, out_len, out_err);"));
    assert!(c.contains("uint8_t* run = event_bus_alloc((size_t)len);"));
    assert!(c.contains("return (event_bus_bus_Store*)(intptr_t)rv;"));
    // A call that can't reach the JVM still releases the adopted object.
    assert!(c.contains(
        "        event_bus_bus_Store_destroy(p_home);\n        return (event_bus_bus_Store*)0;"
    ));
}

#[test]
fn throwing_callbacks_report_their_domain_error() {
    let files = render(&KotlinConfig::default());
    let bridge = file(&files, "JniBridge.kt");
    assert!(bridge.contains("fail(_err, _e, _e as? BusException)"));
    assert!(bridge.contains("error_set(err, typed.code, encodeUtf8(typed.message ?: \"\"))"));
    assert!(bridge.contains("typed.encodePayload()?.let { error_set_payload(err, it) }"));
    assert!(bridge.contains("error_set(err, -4, encodeUtf8(error.message ?: error.toString()))"));
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains(
        "override fun encodePayload(): ByteArray = encodeBuffer { _w ->\n            _w.writeString(key)\n            _w.writeU16(tries)\n        }"
    ));
    assert!(bus.contains("Throw a [BusException] to report a typed error to the library."));
}

#[test]
fn optional_callbacks_pass_a_null_vtable() {
    let files = render(&KotlinConfig::default());
    let bus = file(&files, "Bus.kt");
    assert!(bus.contains("fun watch(sub: Subscriber?) {"));
    assert!(bus.contains("JniBridge.bus_Store_watch(_self, sub)"));
    let bridge = file(&files, "JniBridge.kt");
    assert!(
        bridge.contains("@JvmStatic external fun bus_Store_watch(_self: Long, sub: Subscriber?)")
    );
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains(
        "p_sub != NULL ? (void*)(*env)->NewGlobalRef(env, p_sub) : NULL, p_sub != NULL ? &Jni_bus_Subscriber_vtable : NULL, &err);"
    ));
}

#[test]
fn producer_threads_detach_on_every_platform() {
    let files = render(&KotlinConfig::default());
    let c = file(&files, "event_bus_jni.c");
    assert!(c.contains("pthread_key_create(&Jni_env_key, Jni_thread_exit) == 0;"));
    assert!(c.contains("Jni_env_slot = FlsAlloc(Jni_thread_exit);"));
    assert!(c.contains("*detach = !Jni_remember_attach();"));
    assert!(c.contains("Jni_env_done(detach);"));
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
    let artifacts = KotlinGenerator
        .package(&model, &ctx, &KotlinConfig::default())
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
    let artifacts = KotlinGenerator
        .package(
            &model,
            &PackageContext::new(&partial),
            &KotlinConfig::default(),
        )
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
