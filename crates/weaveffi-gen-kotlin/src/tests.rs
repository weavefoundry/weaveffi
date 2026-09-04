//! Unit tests rendering an IR fixture through the generator: the fixture
//! covers an interface (constructor, methods, statics), a nullable interface
//! parameter and return, a record carrying an interface, iterators over
//! objects and records, a callback interface with buffered, object, nullable
//! object, and enum traffic, and an async callable.

use camino::{Utf8Path, Utf8PathBuf};
use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{FileContent, PackageContext};
use weaveffi_core::platform::{BinarySet, NativeBinary, Platform};
use weaveffi_core::resolved::ResolvedApi;

use crate::{KotlinConfig, KotlinGenerator};

const FIXTURE: &str = r#"
version: "0.9.0"
modules:
  - name: bus
    enums:
      - name: Level
        variants:
          - { name: Low, value: 0 }
          - { name: High, value: 1 }
    structs:
      - name: Event
        doc: A record carrying an object
        fields:
          - { name: id, type: i64 }
          - { name: source, type: Store }
          - { name: parent, type: "Store?" }
    callback_interfaces:
      - name: Subscriber
        doc: Consumer-implemented event sink
        methods:
          - name: on_event
            params:
              - { name: event, type: Event }
              - { name: store, type: Store }
              - { name: maybe, type: "Store?" }
              - { name: note, type: string }
              - { name: level, type: Level }
            return: bool
          - name: pick_level
            params:
              - { name: hint, type: i32 }
            return: Level
          - name: on_close
            params: []
    errors:
      name: BusError
      codes:
        - { name: Closed, code: 1, message: "Bus is closed" }
    interfaces:
      - name: Store
        doc: A reference-counted object
        constructors:
          - name: new
            params:
              - { name: name, type: string }
        methods:
          - name: describe
            params: []
            return: string
          - name: parent
            params: []
            return: "Store?"
          - name: adopt
            params:
              - { name: other, type: "Store?" }
          - name: subscribe
            params:
              - { name: sub, type: Subscriber }
            throws: true
          - name: events
            params: []
            return: "iter<Event>"
          - name: children
            params: []
            return: "iter<Store>"
        statics:
          - name: open_async
            params:
              - { name: name, type: string }
            return: Store
            async: true
    functions:
      - name: publish
        params:
          - { name: store, type: Store }
          - { name: sub, type: Subscriber }
        return: Event
"#;

fn api() -> ResolvedApi {
    let api = weaveffi_ir::parse::parse_api_str(FIXTURE, "yaml").expect("fixture parses");
    weaveffi_core::validate::validate_api(api, None).expect("fixture validates")
}

fn render() -> (String, String) {
    let api = api();
    let model = BindingModel::build(&api, "weaveffi");
    let files = KotlinGenerator.files(&api, &model, Utf8Path::new("out"), &KotlinConfig::default());
    let find = |name: &str| {
        files
            .iter()
            .find(|f| f.path.as_str().ends_with(name))
            .unwrap_or_else(|| {
                panic!(
                    "{name} missing from {:?}",
                    files.iter().map(|f| &f.path).collect::<Vec<_>>()
                )
            })
            .contents
            .clone()
    };
    (find("WeaveFFI.kt"), find("weaveffi_jni.c"))
}

#[test]
fn layout_is_a_kotlin_gradle_module() {
    let api = api();
    let model = BindingModel::build(&api, "weaveffi");
    let files = KotlinGenerator.files(&api, &model, Utf8Path::new("out"), &KotlinConfig::default());
    // `files()` joins with the host separator; the orchestrator's
    // `output_files` normalizes to `/` later, so do the same here for Windows.
    let paths: Vec<String> = files
        .iter()
        .map(|f| f.path.as_str().replace('\\', "/"))
        .collect();
    assert_eq!(
        paths,
        [
            "out/kotlin/settings.gradle.kts",
            "out/kotlin/build.gradle.kts",
            "out/kotlin/src/main/kotlin/com/weaveffi/WeaveFFI.kt",
            "out/kotlin/src/main/cpp/CMakeLists.txt",
            "out/kotlin/src/main/cpp/weaveffi_jni.c",
        ]
    );
    assert_eq!(KotlinGenerator.name(), "kotlin");
}

#[test]
fn interface_wrapper_is_cleaner_backed_and_exposes_clone() {
    let (kt, c) = render();
    // The raw-handle constructor is private so a `new(Long)` exposed as
    // `operator fun invoke(Long)` can't be shadowed by it inside the module;
    // the generated code adopts handles through `fromHandle` instead.
    assert!(kt.contains("class Store private constructor(handle: Long) : AutoCloseable {"));
    assert!(kt.contains("internal fun fromHandle(handle: Long): Store = Store(handle)"));
    assert!(!kt.contains("internal constructor(handle: Long) : AutoCloseable"));
    assert!(kt.contains("private val ref = WeaveNativeRef(handle) { h -> nativeDestroy(h) }"));
    assert!(kt.contains("private val cleanable = weaveCleaner.register(this, ref)"));
    assert!(kt.contains("internal fun cloneHandle(): Long = nativeClone(ref.get())"));
    assert!(kt.contains("override fun close() {\n        cleanable.clean()\n    }"));
    assert!(kt.contains("@JvmStatic private external fun nativeClone(handle: Long): Long"));
    assert!(kt.contains(
        "internal val weaveCleaner: java.lang.ref.Cleaner = java.lang.ref.Cleaner.create()"
    ));
    assert!(!kt.contains("finalize()"));
    assert!(
        c.contains("Java_com_weaveffi_Store_nativeClone(JNIEnv* env, jclass clazz, jlong handle)")
    );
    assert!(c.contains("return (jlong)(intptr_t)weaveffi_bus_Store_clone((const weaveffi_bus_Store*)(intptr_t)handle);"));
    assert!(c.contains("weaveffi_bus_Store_destroy((weaveffi_bus_Store*)(intptr_t)handle);"));
}

#[test]
fn nullable_interface_maps_to_nullable_wrapper() {
    let (kt, c) = render();
    // Return: the JNI layer boxes the owned pointer as `Long?`; the wrapper adopts it.
    assert!(kt.contains("@JvmStatic private external fun nativeParent(selfHandle: Long): Long?"));
    assert!(
        kt.contains("fun parent(): Store? = nativeParent(handle)?.let { Store.fromHandle(it) }")
    );
    assert!(c.contains("if (rv == NULL) { return NULL; }"));
    // Parameter: the wrapper lends its handle, null crosses as null.
    assert!(kt.contains("fun adopt(other: Store?) { nativeAdopt(handle, other?.handle) }"));
    assert!(c.contains("int64_t other_val = 0;"));
    assert!(c.contains("(const weaveffi_bus_Store*)(intptr_t)other_val"));
}

#[test]
fn record_with_object_field_uses_object_tokens() {
    let (kt, _) = render();
    assert!(kt.contains("data class Event(val id: Long, val source: Store, val parent: Store?)"));
    // Encoding clones so the wrapper keeps its own reference; decoding adopts.
    assert!(kt.contains("w.writeI64(v.source.cloneHandle())"));
    assert!(kt.contains("w.writeOptional(v.parent) { v0 -> w.writeI64(v0.cloneHandle()) }"));
    assert!(kt.contains("Store.fromHandle(r.readObject())"));
    assert!(kt.contains("r.readOptional { Store.fromHandle(r.readObject()) }"));
    assert!(kt.contains("fun readObject(): Long {"));
}

#[test]
fn iterators_are_lazy_cleaner_backed_classes() {
    let (kt, c) = render();
    assert!(kt.contains("class BusStoreEventsIterator internal constructor(handle: Long) : Iterator<Event>, AutoCloseable {"));
    assert!(kt.contains("class BusStoreChildrenIterator internal constructor(handle: Long) : Iterator<Store>, AutoCloseable {"));
    assert!(kt.contains("val slot = nativeNext(ref.get())"));
    assert!(kt.contains("return Store.fromHandle(raw as Long)"));
    assert!(kt.contains("return weaveDecode((raw as ByteArray)) { r -> unpackEvent(r) }"));
    assert!(
        kt.contains("fun events(): Iterator<Event> = BusStoreEventsIterator(nativeEvents(handle))")
    );
    assert!(c.contains("Java_com_weaveffi_BusStoreChildrenIterator_nativeNext"));
    assert!(c.contains("weaveffi_bus_Store_EventsIterator_destroy"));
}

#[test]
fn callback_interface_renders_interface_and_dispatch_object() {
    let (kt, _) = render();
    assert!(kt.contains("interface Subscriber {"));
    assert!(kt.contains(
        "fun onEvent(event: Event, store: Store, maybe: Store?, note: String, level: Level): Boolean"
    ));
    assert!(kt.contains("fun pickLevel(hint: Int): Level"));
    assert!(kt.contains("fun onClose()"));
    assert!(kt.contains("internal object SubscriberJni {"));
    // The shim decodes the buffer, adopts both object references, and lowers the enum.
    assert!(kt.contains(
        "@JvmStatic fun onEvent(impl: Subscriber, event: ByteArray, store: Long, maybe: Long, note: String, level: Int): Boolean = impl.onEvent(weaveDecode(event) { r -> unpackEvent(r) }, Store.fromHandle(store), maybe.takeIf { it != 0L }?.let { Store.fromHandle(it) }, note, Level.fromValue(level))"
    ));
    assert!(kt.contains(
        "@JvmStatic fun pickLevel(impl: Subscriber, hint: Int): Int = impl.pickLevel(hint).value"
    ));
    assert!(kt.contains("@JvmStatic fun onClose(impl: Subscriber) { impl.onClose() }"));
    // Passing an implementation needs no wrapper split: the object crosses as itself.
    assert!(kt.contains("fun subscribe(sub: Subscriber) { nativeSubscribe(handle, sub) }"));
    assert!(kt.contains(
        "@JvmStatic private external fun publishJni(store: Long, sub: Subscriber): ByteArray"
    ));
}

#[test]
fn callback_interface_shim_pins_global_ref_and_uses_static_vtable() {
    let (_, c) = render();
    // JNI_OnLoad caches the VM, the dispatch class, and every method ID.
    assert!(c.contains("static JavaVM* weaveffi_jni_vm = NULL;"));
    assert!(c.contains("JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* reserved) {"));
    assert!(c.contains("if (weaveffi_abi_version() != 2u) {"));
    assert!(c.contains("FindClass(env, \"com/weaveffi/SubscriberJni\")"));
    assert!(c.contains(
        "GetStaticMethodID(env, weaveffi_bus_Subscriber_jni_cls, \"onEvent\", \"(Lcom/weaveffi/Subscriber;[BJJLjava/lang/String;I)Z\");"
    ));
    assert!(c.contains(
        "GetStaticMethodID(env, weaveffi_bus_Subscriber_jni_cls, \"pickLevel\", \"(Lcom/weaveffi/Subscriber;I)I\");"
    ));
    assert!(c.contains(
        "GetStaticMethodID(env, weaveffi_bus_Subscriber_jni_cls, \"onClose\", \"(Lcom/weaveffi/Subscriber;)V\");"
    ));
    // Trampolines attach, convert, call, and report foreign failures.
    // The trampoline signature is the header's vtable entry: object arguments
    // arrive as owned (non-const) pointers the shim adopts.
    assert!(c.contains("static bool weaveffi_bus_Subscriber_jni_on_event(void* ctx, const uint8_t* event_ptr, size_t event_len, weaveffi_bus_Store* store, weaveffi_bus_Store* maybe, const char* note, weaveffi_bus_Level level, weaveffi_error* out_err) {"));
    assert!(c.contains("int attached = weaveffi_jni_attach(&env);"));
    assert!(c.contains("jbyteArray _a0 = (*env)->NewByteArray(env, (jsize)event_len);"));
    assert!(c.contains("jlong _a1 = (jlong)(intptr_t)store;"));
    assert!(c.contains("jlong _a2 = (jlong)(intptr_t)maybe;"));
    assert!(c.contains("jstring _a3 = weaveffi_jni_utf8_to_string(env, note);"));
    assert!(c.contains("jint _a4 = (jint)level;"));
    assert!(c.contains("jboolean _rv = (*env)->CallStaticBooleanMethod(env, weaveffi_bus_Subscriber_jni_cls, weaveffi_bus_Subscriber_jni_mid_on_event, (jobject)ctx, _a0, _a1, _a2, _a3, _a4);"));
    assert!(c.contains("weaveffi_jni_report_foreign(env, out_err);"));
    assert!(c.contains("weaveffi_error_set(out_err, -4, msg != NULL ? msg : \"callback interface implementation threw\");"));
    assert!(c.contains("return _rv == JNI_TRUE;"));
    assert!(c.contains("jint _rv = (*env)->CallStaticIntMethod(env, weaveffi_bus_Subscriber_jni_cls, weaveffi_bus_Subscriber_jni_mid_pick_level, (jobject)ctx, _a0);"));
    assert!(c.contains("weaveffi_jni_detach(attached);"));
    // `free` deletes the global ref; the vtable is one static value.
    assert!(c.contains("static void weaveffi_bus_Subscriber_jni_free(void* ctx) {"));
    assert!(c.contains("(*env)->DeleteGlobalRef(env, (jobject)ctx);"));
    assert!(c.contains("static const weaveffi_bus_Subscriber_vtable weaveffi_bus_Subscriber_jni_vtable = { weaveffi_bus_Subscriber_jni_on_event, weaveffi_bus_Subscriber_jni_pick_level, weaveffi_bus_Subscriber_jni_on_close, weaveffi_bus_Subscriber_jni_free };"));
    // The export pins the implementation and passes ctx + vtable.
    assert!(c.contains("jobject sub_ref = (*env)->NewGlobalRef(env, sub);"));
    assert!(c.contains("weaveffi_bus_Store_subscribe((const weaveffi_bus_Store*)(intptr_t)selfHandle, (void*)sub_ref, &weaveffi_bus_Subscriber_jni_vtable, &err);"));
    // The throwing method dispatches to the domain thrower.
    assert!(c.contains("throw_weaveffi_bus_BusError(env, &err);"));
}

#[test]
fn strings_cross_as_standard_utf8_not_modified_utf8() {
    let (_, c) = render();
    // Both converters are emitted once, ahead of every user.
    assert!(c.contains("#include <string.h>"));
    assert!(c.contains("static char* weaveffi_jni_string_to_utf8(JNIEnv* env, jstring s) {"));
    assert!(c.contains("static jstring weaveffi_jni_utf8_to_string(JNIEnv* env, const char* s) {"));
    assert!(
        c.find("weaveffi_jni_string_to_utf8").unwrap() < c.find("throw_weaveffi_error").unwrap()
    );
    // A string parameter is copied as standard UTF-8 and freed after the call;
    // a string return is rebuilt from UTF-8. JNI's modified-UTF-8 entry points
    // never appear, since they mangle supplementary characters.
    assert!(c.contains("char* name_chars = weaveffi_jni_string_to_utf8(env, name);"));
    assert!(c.contains("free(name_chars);"));
    assert!(c.contains("jstring out = weaveffi_jni_utf8_to_string(env, rv);"));
    assert!(c.contains("jstring jmsg = weaveffi_jni_utf8_to_string(env, msg);"));
    assert!(c.contains("char* msg = weaveffi_jni_string_to_utf8(env, jmsg);"));
    assert!(!c.contains("->GetStringUTFChars("));
    assert!(!c.contains("->ReleaseStringUTFChars("));
    assert!(!c.contains("->NewStringUTF("));
}

#[test]
fn async_uses_cached_vm_and_adopts_object_result() {
    let (kt, c) = render();
    assert!(kt.contains("suspend fun openAsync(name: String): Store {"));
    assert!(kt.contains("return Store.fromHandle(raw)"));
    assert!(c.contains("static void weaveffi_bus_Store_open_async_jni_cb(void* context, weaveffi_error* err, weaveffi_bus_Store* result) {"));
    assert!(c.contains("weaveffi_jni_handle_uncaught(env);"));
    assert!(c.contains("(jlong)(intptr_t)result);"));
    assert!(!c.contains("GetJavaVM"));
}

#[test]
fn no_arena_or_listener_bindings_remain() {
    let (kt, c) = render();
    for stale in ["arena", "listener", "Listener", "pthread"] {
        assert!(!kt.contains(stale), "Kotlin still mentions {stale}");
        assert!(!c.contains(stale), "C shim still mentions {stale}");
    }
    // Every class touching natives goes through the single loader object.
    assert!(kt.contains("init { WeaveNativeLibrary.ensureLoaded() }"));
    assert_eq!(kt.matches("System.loadLibrary(").count(), 1);
    assert!(kt.contains("init { System.loadLibrary(\"weaveffi\") }"));
}

#[test]
fn package_lays_out_an_aar_style_module() {
    let api = api();
    let model = BindingModel::build(&api, "weaveffi");
    let mut binaries = BinarySet::new("bus");
    binaries.binaries.push(NativeBinary {
        platform: Platform::AndroidArm64,
        source: Utf8PathBuf::from("/prebuilt/android-arm64/libbus.so"),
    });
    binaries.binaries.push(NativeBinary {
        platform: Platform::MacosArm64,
        source: Utf8PathBuf::from("/prebuilt/darwin-arm64/libbus.dylib"),
    });
    binaries.binaries.push(NativeBinary {
        platform: Platform::Wasm32,
        source: Utf8PathBuf::from("/prebuilt/wasm32/bus.wasm"),
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
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "out/kotlin/settings.gradle.kts",
            "out/kotlin/build.gradle.kts",
            "out/kotlin/README.md",
            "out/kotlin/src/main/kotlin/com/weaveffi/WeaveFFI.kt",
            "out/kotlin/src/main/cpp/CMakeLists.txt",
            "out/kotlin/src/main/cpp/weaveffi_jni.c",
            "out/kotlin/src/main/cpp/include/weaveffi.h",
            "out/kotlin/src/main/jniLibs/arm64-v8a/libbus.so",
            "out/kotlin/src/main/resources/natives/darwin-arm64/libbus.dylib",
        ]
    );
    let text = |name: &str| match &files
        .iter()
        .find(|f| f.path.as_str().ends_with(name))
        .unwrap()
        .content
    {
        FileContent::Text(t) => t.clone(),
        FileContent::Copy(_) => panic!("{name} is a binary"),
    };
    let kt = text("WeaveFFI.kt");
    assert!(kt.contains("private const val LIB = \"bus\""));
    assert!(kt.contains("private const val JNI_LIB = \"bus_jni\""));
    assert!(kt.contains("System.loadLibrary(JNI_LIB)"));
    assert!(kt.contains("getResourceAsStream(\"/natives/$id/$name\")"));
    let cmake = text("CMakeLists.txt");
    assert!(cmake.contains("add_library(bus_jni SHARED weaveffi_jni.c)"));
    assert!(cmake.contains("../jniLibs/${ANDROID_ABI}/libbus.so"));
    let gradle = text("build.gradle.kts");
    assert!(gradle.contains("namespace = \"com.weaveffi\""));
    assert!(gradle.contains("minSdk = 33"));
    assert!(gradle.contains("jniLibs.srcDirs(\"src/main/jniLibs\")"));
    let header = text("weaveffi.h");
    assert!(header.contains("typedef struct weaveffi_bus_Subscriber_vtable {"));
    assert!(header.contains("weaveffi_bus_Store_clone"));
    let readme = text("README.md");
    assert!(readme.contains("- `src/main/jniLibs/arm64-v8a/libbus.so`"));
    assert!(readme.contains("- `src/main/resources/natives/darwin-arm64/libbus.dylib`"));
    assert!(!readme.contains("wasm32"));
}
