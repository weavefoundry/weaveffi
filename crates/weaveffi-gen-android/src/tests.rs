//! Unit tests for the Android backend: rendering assertions over the Kotlin
//! wrapper and the JNI C bridge, plus file-set and config-variant checks.

use camino::Utf8Path;
use weaveffi_core::codegen::common::pascal_case;
use weaveffi_core::codegen::Generator;
use weaveffi_core::model::BindingModel;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
    StructField, TypeRef,
};

use crate::types::{jni_param_type, kotlin_jni_type, kotlin_type};
use crate::{AndroidConfig, AndroidGenerator};

fn make_api(modules: Vec<Module>) -> ResolvedApi {
    ResolvedApi::assume_resolved(Api {
        version: "0.7.0".to_string(),
        modules,
        generators: None,
        package: None,
    })
}

/// Test-local shim mirroring the driver: build the model once and hand it
/// to the renderer (production code never calls `BindingModel::build`).
fn render_kotlin(api: &ResolvedApi, package: &str, strip: bool, input_basename: &str) -> String {
    super::render_kotlin(
        &BindingModel::build(api, "weaveffi"),
        package,
        strip,
        input_basename,
    )
}

/// Test-local shim for the JNI renderer; `c_prefix` seeds the model the
/// same way the driver's global prefix does.
fn render_jni_c(
    api: &ResolvedApi,
    package: &str,
    strip: bool,
    input_basename: &str,
    c_prefix: &str,
) -> String {
    super::render_jni_c(
        &BindingModel::build(api, c_prefix),
        package,
        strip,
        input_basename,
    )
}

fn make_struct_api() -> ResolvedApi {
    make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![
                StructField {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }])
}

fn enum_variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
    EnumVariant {
        name: name.to_string(),
        value,
        doc: None,
        fields,
    }
}

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.to_string(),
        ty,
        doc: None,
    }
}

/// The `shapes` conformance sample in its already-resolved IR form: a rich
/// (algebraic) enum `Shape`, a plain enum `Channel`, and free functions that
/// take/return the rich enum (lowered to an opaque `Struct` pointer).
fn make_shapes_api() -> ResolvedApi {
    make_api(vec![Module {
        name: "shapes".to_string(),
        enums: vec![
            EnumDef {
                name: "Shape".to_string(),
                doc: None,
                variants: vec![
                    enum_variant("Empty", 0, vec![]),
                    enum_variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
                    enum_variant(
                        "Rectangle",
                        2,
                        vec![field("width", TypeRef::F32), field("height", TypeRef::F32)],
                    ),
                    enum_variant(
                        "Labeled",
                        3,
                        vec![
                            field("label", TypeRef::StringUtf8),
                            field("count", TypeRef::U8),
                        ],
                    ),
                ],
            },
            EnumDef {
                name: "Channel".to_string(),
                doc: None,
                variants: vec![
                    enum_variant("Red", 0, vec![]),
                    enum_variant("Green", 1, vec![]),
                    enum_variant("Blue", 2, vec![]),
                ],
            },
        ],
        // Rich-enum references are resolved to opaque `Struct` pointers.
        functions: vec![
            Function {
                name: "describe".to_string(),
                params: vec![Param {
                    name: "shape".to_string(),
                    ty: TypeRef::RichEnum("Shape".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "scale".to_string(),
                params: vec![
                    Param {
                        name: "shape".to_string(),
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "factor".to_string(),
                        ty: TypeRef::F64,
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::RichEnum("Shape".into())),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "sum_bytes".to_string(),
                params: vec![Param {
                    name: "values".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::U8)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::U64),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }])
}

// --- Rich (algebraic) enum tests ---

#[test]
fn kotlin_rich_enum_is_sealed_class_not_plain_enum() {
    let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
    assert!(
        kt.contains("sealed class Shape {"),
        "rich enum must be a sealed class: {kt}"
    );
    // It must NOT degrade into a plain `enum class Shape(...)`, and it has
    // no native handle or disposal surface.
    assert!(
        !kt.contains("enum class Shape("),
        "rich enum must not be emitted as a plain enum class: {kt}"
    );
    assert!(
        !kt.contains("class Shape internal constructor"),
        "rich enum must not be a handle-wrapper class: {kt}"
    );
    // The plain sibling enum `Channel` is still a normal enum class.
    assert!(
        kt.contains("enum class Channel(val value: Int) {"),
        "plain enum must still be a plain enum class: {kt}"
    );
}

#[test]
fn kotlin_rich_enum_variant_subtypes() {
    let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
    for expected in [
        "object Empty : Shape()",
        "data class Circle(val radius: Double) : Shape()",
        "data class Rectangle(val width: Float, val height: Float) : Shape()",
        "data class Labeled(val label: String, val count: Byte) : Shape()",
    ] {
        assert!(kt.contains(expected), "missing variant `{expected}`: {kt}");
    }
}

#[test]
fn kotlin_rich_enum_pack_writes_tag_then_fields() {
    let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
    assert!(
        kt.contains("internal fun packShape(w: WeaveBufferWriter, v: Shape) {"),
        "missing packShape codec: {kt}"
    );
    assert!(
        kt.contains("is Shape.Empty -> w.writeI32(0)"),
        "unit variant must write only its tag: {kt}"
    );
    let circle = kt.split("is Shape.Circle -> {").nth(1).unwrap();
    assert!(
        circle.contains("w.writeI32(1)") && circle.contains("w.writeF64(v.radius)"),
        "Circle must write tag 1 then its f64 field: {kt}"
    );
    let labeled = kt.split("is Shape.Labeled -> {").nth(1).unwrap();
    assert!(
        labeled.contains("w.writeI32(3)")
            && labeled.contains("w.writeString(v.label)")
            && labeled.contains("w.writeI8(v.count)"),
        "Labeled must write tag 3 then its fields in order: {kt}"
    );
}

#[test]
fn kotlin_rich_enum_unpack_dispatches_on_tag() {
    let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
    assert!(
        kt.contains(
            "internal fun unpackShape(r: WeaveBufferReader): Shape = when (val tag = r.readI32()) {"
        ),
        "missing unpackShape codec: {kt}"
    );
    for expected in [
        "0 -> Shape.Empty",
        "1 -> Shape.Circle(r.readF64())",
        "2 -> Shape.Rectangle(r.readF32(), r.readF32())",
        "3 -> Shape.Labeled(r.readString(), r.readI8())",
    ] {
        assert!(
            kt.contains(expected),
            "missing unpack arm `{expected}`: {kt}"
        );
    }
    assert!(
        kt.contains("unknown Shape tag $tag"),
        "unpack must reject unknown tags: {kt}"
    );
}

#[test]
fn kotlin_rich_enum_has_no_native_surface() {
    let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
    for forbidden in [
        "nativeNewCircle",
        "nativeTag",
        "nativeGetCircleRadius",
        "Shape.nativeDestroy",
    ] {
        assert!(
            !kt.contains(forbidden),
            "rich enums have no C symbols; found `{forbidden}`: {kt}"
        );
    }
}

#[test]
fn kotlin_rich_enum_function_marshalling() {
    let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
    // A rich enum passed in is packed into a ByteArray; one returned is
    // decoded from the ByteArray the JNI shim copies back.
    assert!(
        kt.contains(
            "@JvmStatic fun shapesDescribe(shape: Shape): String = shapesDescribeJni(weaveEncode { w -> packShape(w, shape) })"
        ),
        "rich-enum param must marshal via packShape: {kt}"
    );
    assert!(
        kt.contains(
            "@JvmStatic fun shapesScale(shape: Shape, factor: Double): Shape = weaveDecode(shapesScaleJni(weaveEncode { w -> packShape(w, shape) }, factor)) { r -> unpackShape(r) }"
        ),
        "rich-enum return must decode via unpackShape: {kt}"
    );
    assert!(
        kt.contains(
            "@JvmStatic private external fun shapesScaleJni(shape: ByteArray, factor: Double): ByteArray"
        ),
        "JNI external must carry the rich enum as a ByteArray: {kt}"
    );
}

#[test]
fn jni_rich_enum_param_pins_and_releases_buffer() {
    let jni = render_jni_c(
        &make_shapes_api(),
        "com.weaveffi",
        false,
        "shapes.yml",
        "weaveffi",
    );
    let describe = jni
        .split("Java_com_weaveffi_WeaveFFI_shapesDescribeJni")
        .nth(1)
        .unwrap();
    assert!(
        describe.contains("jbyte* shape_elems = (*env)->GetByteArrayElements(env, shape, NULL);"),
        "buffered param must pin the ByteArray: {jni}"
    );
    assert!(
        describe.contains(
            "weaveffi_shapes_describe((const uint8_t*)shape_elems, (size_t)shape_len, &err)"
        ),
        "buffered param must pass borrowed (ptr, len): {jni}"
    );
    assert!(
        describe.contains("(*env)->ReleaseByteArrayElements(env, shape, shape_elems, JNI_ABORT);"),
        "buffered param must be released without copy-back: {jni}"
    );
}

#[test]
fn jni_rich_enum_return_copies_and_frees_buffer() {
    let jni = render_jni_c(
        &make_shapes_api(),
        "com.weaveffi",
        false,
        "shapes.yml",
        "weaveffi",
    );
    let scale = jni
        .split("Java_com_weaveffi_WeaveFFI_shapesScaleJni")
        .nth(1)
        .unwrap();
    assert!(
        scale.contains("const uint8_t* rv = weaveffi_shapes_scale((const uint8_t*)shape_elems, (size_t)shape_len, (double)factor, &out_len, &err);"),
        "buffered return must thread the out_len slot: {jni}"
    );
    assert!(
        scale.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
        "buffered return must copy into a ByteArray: {jni}"
    );
    assert!(
        scale.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
        "the producer allocation must be freed after copying: {jni}"
    );
}

#[test]
fn jni_rich_enum_has_no_object_bridge() {
    let jni = render_jni_c(
        &make_shapes_api(),
        "com.weaveffi",
        false,
        "shapes.yml",
        "weaveffi",
    );
    for forbidden in [
        "Java_com_weaveffi_Shape_",
        "weaveffi_shapes_Shape_tag",
        "weaveffi_shapes_Shape_Circle_new",
        "weaveffi_shapes_Shape_destroy",
    ] {
        assert!(
            !jni.contains(forbidden),
            "rich enums have no C symbols; found `{forbidden}`: {jni}"
        );
    }
}

#[test]
fn rich_enum_appears_in_generated_files() {
    let api = make_shapes_api();
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    AndroidGenerator
        .generate(&api, out, &AndroidConfig::default())
        .unwrap();
    let kotlin =
        std::fs::read_to_string(out.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .unwrap();
    assert!(
        kotlin.contains("sealed class Shape {"),
        "rich enum sealed class missing from generated Kotlin file"
    );
    assert!(
        kotlin.contains("internal fun packShape(") && kotlin.contains("internal fun unpackShape("),
        "rich enum codecs missing from generated Kotlin file"
    );
    let jni = std::fs::read_to_string(out.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
    assert!(
        jni.contains("Java_com_weaveffi_WeaveFFI_scaleJni")
            && jni.contains("weaveffi_shapes_scale((const uint8_t*)shape_elems"),
        "buffered rich enum marshalling missing from generated JNI file"
    );
}

#[test]
fn listeners_generate_kotlin_and_jni() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".to_string(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnMessage".to_string(),
            doc: None,
            params: vec![Param {
                name: "message".to_string(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
        }],
        listeners: vec![ListenerDef {
            name: "message_listener".to_string(),
            event_callback: "OnMessage".to_string(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", false, "weaveffi.yml");
    assert!(
        kt.contains(
            "@JvmStatic external fun eventsRegisterMessageListener(callback: (String) -> Unit): Long"
        ),
        "register external missing: {kt}"
    );
    assert!(
        kt.contains("@JvmStatic external fun eventsUnregisterMessageListener(id: Long)"),
        "unregister external missing: {kt}"
    );

    let jni = render_jni_c(&api, "com.weaveffi", false, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("#include <pthread.h>"),
        "registry must be mutex-guarded: {jni}"
    );
    assert!(
        jni.contains("static void weaveffi_events_OnMessage_fn_jni_tramp(const char* message, void* context)"),
        "trampoline missing: {jni}"
    );
    assert!(
        jni.contains("AttachCurrentThread"),
        "trampoline must attach producer threads: {jni}"
    );
    assert!(
        jni.contains("\"invoke\", \"(Ljava/lang/Object;)Ljava/lang/Object;\""),
        "must call the erased Function1.invoke: {jni}"
    );
    assert!(
        jni.contains("Java_com_weaveffi_WeaveFFI_eventsRegisterMessageListener"),
        "register JNI export missing: {jni}"
    );
    assert!(
        jni.contains("weaveffi_events_register_message_listener(weaveffi_events_OnMessage_fn_jni_tramp, ctx)"),
        "register must call the C ABI register symbol: {jni}"
    );
    assert!(
        jni.contains("NewGlobalRef"),
        "callback must be pinned with a global ref: {jni}"
    );
    assert!(
        jni.contains("DeleteGlobalRef"),
        "unregister must unpin the callback: {jni}"
    );
}

#[test]
fn list_of_string_return_is_buffered() {
    let api = make_api(vec![Module {
        name: "m".to_string(),
        functions: vec![Function {
            name: "all_names".to_string(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
        "string-list return crosses as one value buffer: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun allNames(): List<String>"),
        "kotlin surface must be List<String>: {kt}"
    );
    assert!(
        kt.contains("weaveDecode(allNamesJni()) { r -> r.readList { r.readString() } }"),
        "the wrapper must decode the buffered list: {kt}"
    );
}

/// A single-module API with one free function, for return-marshalling
/// tests.
fn make_fn_api(
    name: &str,
    params: Vec<Param>,
    returns: Option<TypeRef>,
    throws: bool,
) -> ResolvedApi {
    make_api(vec![Module {
        name: "m".to_string(),
        functions: vec![Function {
            name: name.to_string(),
            params,
            returns,
            doc: None,
            r#async: false,
            throws,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }])
}

#[test]
fn buffered_list_return_frees_producer_buffer() {
    let api = make_fn_api(
        "all_names",
        vec![],
        Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
        false,
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_m_all_names(&out_len, &err);"),
        "the buffered return threads the trailing out_len slot: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
        "the producer buffer must be freed after copying: {jni}"
    );
}

#[test]
fn optional_scalar_return_is_buffered() {
    let api = make_fn_api(
        "find_age",
        vec![],
        Some(TypeRef::Optional(Box::new(TypeRef::I64))),
        false,
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
        "an optional scalar return crosses as one value buffer: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun findAge(): Long? = weaveDecode(findAgeJni()) { r -> r.readOptional { r.readI64() } }"),
        "the wrapper must decode the optional flag byte plus value: {kt}"
    );
}

#[test]
fn map_return_is_buffered_and_freed() {
    let api = make_fn_api(
        "all_scores",
        vec![],
        Some(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32),
        )),
        false,
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_m_all_scores(&out_len, &err);"),
        "the map return crosses as one value buffer: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
        "the producer buffer must be freed after copying: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun allScores(): Map<String, Int> = weaveDecode(allScoresJni()) { r -> r.readMap({ r.readString() }, { r.readI32() }) }"),
        "the wrapper must decode alternating keys and values: {kt}"
    );
}

#[test]
fn string_param_released_before_error_check() {
    let api = make_fn_api(
        "check",
        vec![Param {
            name: "name".to_string(),
            ty: TypeRef::StringUtf8,
            mutable: false,
            doc: None,
        }],
        Some(TypeRef::I32),
        false,
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    let release = jni
        .find("ReleaseStringUTFChars")
        .expect("string param must be released");
    let err_check = jni
        .find("if (err.code != 0)")
        .expect("error check must be emitted");
    assert!(
        release < err_check,
        "the borrowed string must be released before the error check so \
         error paths cannot leak it: {jni}"
    );
}

#[test]
fn iterator_fn_emits_lazy_kotlin_wrapper() {
    let api = make_fn_api(
        "stream_names",
        vec![],
        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        false,
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun streamNames(): Iterator<String> = MStreamNamesIterator(streamNamesJni())"),
        "the public surface must adopt the handle into the iterator class: {kt}"
    );
    assert!(
        kt.contains("@JvmStatic private external fun streamNamesJni(): Long"),
        "the native launcher must return the raw handle: {kt}"
    );
    assert!(
        kt.contains(
            "class MStreamNamesIterator internal constructor(private var handle: Long) : Iterator<String>, java.io.Closeable {"
        ),
        "missing lazy iterator wrapper class: {kt}"
    );
    assert!(
        kt.contains("val slot = nativeNext(handle)"),
        "hasNext must pull exactly one element into the lookahead slot: {kt}"
    );
    assert!(
        kt.contains("override fun close() {") && kt.contains("protected fun finalize() {"),
        "the iterator must destroy its handle via close()/finalize(): {kt}"
    );
    assert!(
        !kt.contains("ArrayList") && !kt.contains("toList"),
        "the iterator must not drain into a list: {kt}"
    );
}

#[test]
fn iterator_fn_emits_jni_launch_next_destroy() {
    let api = make_fn_api(
        "stream_names",
        vec![],
        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        false,
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("return (jlong)(intptr_t)_iter;"),
        "the launcher must hand the raw iterator handle to Kotlin: {jni}"
    );
    assert!(
        jni.contains("Java_com_weaveffi_MStreamNamesIterator_nativeNext"),
        "missing per-iterator nativeNext export: {jni}"
    );
    assert!(
        jni.contains("Java_com_weaveffi_MStreamNamesIterator_nativeDestroy"),
        "missing per-iterator nativeDestroy export: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_string(_item);"),
        "each string element must be freed after NewStringUTF: {jni}"
    );
    assert!(
        !jni.contains("java/util/ArrayList") && !jni.contains("while ("),
        "the glue must not drain the iterator eagerly: {jni}"
    );
}

#[test]
fn iterator_record_elements_are_decoded_and_freed() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![Function {
            name: "stream_contacts".to_string(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("return weaveDecode((raw as ByteArray)) { r -> unpackContact(r) }"),
        "record elements must be decoded from the buffered ByteArray: {kt}"
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    let next_start = jni
        .find("Java_com_weaveffi_ContactsStreamContactsIterator_nativeNext")
        .expect("nativeNext export missing");
    let next_end = jni[next_start..]
        .find("\n}\n")
        .map(|i| next_start + i)
        .expect("nativeNext body must close");
    let next_body = &jni[next_start..next_end];
    assert!(
        next_body.contains("jbyteArray _jitem = (*env)->NewByteArray(env, (jsize)_item_len);"),
        "buffered elements must be copied into a ByteArray: {next_body}"
    );
    assert!(
        next_body.contains("weaveffi_free_bytes((uint8_t*)_item, _item_len);"),
        "each buffered element must be freed after copying: {next_body}"
    );
}

#[test]
fn iterator_throws_uses_domain_thrower_per_next() {
    let api = make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![Function {
            name: "scan".to_string(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            doc: None,
            r#async: false,
            throws: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "KvError".to_string(),
            codes: vec![ErrorCode {
                name: "IoFailure".to_string(),
                code: 1,
                message: "IO failure".to_string(),
                doc: None,
                fields: vec![],
            }],
        }),
        modules: vec![],
    }]);
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    let next_start = jni
        .find("Java_com_weaveffi_KvScanIterator_nativeNext")
        .expect("nativeNext export missing");
    assert!(
        jni[next_start..].contains("throw_weaveffi_kv_KvError(env, &err);"),
        "per-next errors on a throwing callable must use the typed domain thrower: {jni}"
    );
}

#[test]
fn listener_exception_policy_routes_to_handler_then_describes() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".to_string(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnMessage".to_string(),
            doc: None,
            params: vec![Param {
                name: "message".to_string(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
        }],
        listeners: vec![ListenerDef {
            name: "message_listener".to_string(),
            event_callback: "OnMessage".to_string(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("weaveffi_jni_handle_uncaught(env);"),
        "the trampoline must route exceptions to the uncaught handler: {jni}"
    );
    assert!(
        jni.contains("(*env)->ExceptionDescribe(env);"),
        "unhandled exceptions must be logged with ExceptionDescribe: {jni}"
    );
    assert!(
        !jni.contains("if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);"),
        "exceptions must never be silently cleared: {jni}"
    );
    assert!(
        jni.contains("JNI_OnLoad")
            && jni.contains("\"dispatchCallbackException\", \"(Ljava/lang/Throwable;)V\""),
        "the handler hook must be cached at load time: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun setCallbackExceptionHandler(handler: ((Throwable) -> Unit)?)"),
        "missing settable exception handler: {kt}"
    );
    assert!(
        kt.contains("logged with their stack trace and dropped"),
        "the listener exception policy must be documented: {kt}"
    );
}

#[test]
fn async_bytes_result_is_copied_then_freed() {
    let api = make_api(vec![Module {
        name: "m".to_string(),
        functions: vec![Function {
            name: "fetch".to_string(),
            params: vec![],
            returns: Some(TypeRef::Bytes),
            doc: None,
            r#async: true,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("const uint8_t* result, size_t result_len"),
        "the callback signature must match the lowered ABI slots: {jni}"
    );
    assert!(
        jni.contains("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);"),
        "the owned buffer must be deep-copied into a ByteArray: {jni}"
    );
    let cb_start = jni.find("_jni_cb(void* context").expect("callback missing");
    let cb_end = jni[cb_start..]
        .find("\n}\n")
        .map(|i| cb_start + i)
        .expect("callback body must close");
    let cb_body = &jni[cb_start..cb_end];
    assert!(
        cb_body.contains("weaveffi_free_bytes((uint8_t*)result, result_len);"),
        "the callback owns the result buffer and must free it after copying: {cb_body}"
    );
    assert!(
        cb_body.contains("weaveffi_error_free(err);"),
        "the callback owns the boxed error and must free it: {cb_body}"
    );
    assert!(
        cb_body.contains("weaveffi_jni_handle_uncaught(env);"),
        "resume-path exceptions must go through the uncaught handler: {cb_body}"
    );
}

#[test]
fn kotlin_struct_is_data_class() {
    let api = make_struct_api();
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("data class Contact(val name: String, val age: Int)"),
        "missing record data class: {kt}"
    );
    assert!(
        !kt.contains("class Contact internal constructor"),
        "records must not be handle-wrapper classes: {kt}"
    );
}

#[test]
fn kotlin_struct_codecs_follow_field_order() {
    let api = make_struct_api();
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("internal fun packContact(w: WeaveBufferWriter, v: Contact) {"),
        "missing packContact codec: {kt}"
    );
    let pack = kt.split("internal fun packContact").nth(1).unwrap();
    let name_at = pack.find("w.writeString(v.name)").expect("name write");
    let age_at = pack.find("w.writeI32(v.age)").expect("age write");
    assert!(
        name_at < age_at,
        "fields must be written in declaration order: {kt}"
    );
    assert!(
        kt.contains("internal fun unpackContact(r: WeaveBufferReader): Contact = Contact("),
        "missing unpackContact codec: {kt}"
    );
    let unpack = kt.split("internal fun unpackContact").nth(1).unwrap();
    let name_read = unpack.find("r.readString(),").expect("name read");
    let age_read = unpack.find("r.readI32(),").expect("age read");
    assert!(
        name_read < age_read,
        "fields must be read in declaration order: {kt}"
    );
}

#[test]
fn kotlin_struct_has_no_native_surface() {
    let api = make_struct_api();
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    for forbidden in [
        "nativeCreate",
        "nativeGetName",
        "ContactBuilder",
        "fun create(name: String",
    ] {
        assert!(
            !kt.contains(forbidden),
            "records have no C symbols or builders; found `{forbidden}`: {kt}"
        );
    }
    let contact_at = kt.find("data class Contact").expect("record class");
    let brand_at = kt.find("open class WeaveFFIException").expect("brand");
    assert!(
        !kt[contact_at..brand_at].contains("close()"),
        "records need no disposal: {kt}"
    );
}

#[test]
fn jni_struct_has_no_object_bridge() {
    let api = make_struct_api();
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    for forbidden in [
        "Java_com_weaveffi_Contact_",
        "weaveffi_contacts_Contact_create",
        "weaveffi_contacts_Contact_destroy",
        "weaveffi_contacts_Contact_get_name",
    ] {
        assert!(
            !jni.contains(forbidden),
            "records have no C symbols; found `{forbidden}`: {jni}"
        );
    }
}

#[test]
fn kotlin_struct_with_bytes_field() {
    let api = make_api(vec![Module {
        name: "storage".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Blob".to_string(),
            doc: None,
            fields: vec![StructField {
                name: "data".to_string(),
                ty: TypeRef::Bytes,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("data class Blob(val data: ByteArray)"),
        "missing bytes-typed record property: {kt}"
    );
    assert!(
        kt.contains("w.writeBytes(v.data)"),
        "bytes field must serialize as a length-prefixed run: {kt}"
    );
    assert!(
        kt.contains("r.readBytes(),"),
        "bytes field must deserialize via readBytes: {kt}"
    );
}

#[test]
fn kotlin_struct_with_nested_struct_field() {
    let api = make_api(vec![Module {
        name: "geo".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Line".to_string(),
            doc: None,
            fields: vec![StructField {
                name: "start".to_string(),
                ty: TypeRef::Record("Point".into()),
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("data class Line(val start: Point)"),
        "missing nested record property: {kt}"
    );
    assert!(
        kt.contains("packPoint(w, v.start)"),
        "nested record must serialize inline through its own codec: {kt}"
    );
    assert!(
        kt.contains("unpackPoint(r),"),
        "nested record must deserialize through its own codec: {kt}"
    );
}

#[test]
fn kotlin_type_for_struct_returns_name() {
    assert_eq!(kotlin_type(&TypeRef::Record("Contact".into())), "Contact");
}

#[test]
fn kotlin_jni_type_for_struct_is_byte_array() {
    assert_eq!(
        kotlin_jni_type(&TypeRef::Record("Contact".into())),
        "ByteArray"
    );
}

#[test]
fn pascal_case_converts_snake_case() {
    assert_eq!(pascal_case("first_name"), "FirstName");
    assert_eq!(pascal_case("name"), "Name");
    assert_eq!(pascal_case("is_active"), "IsActive");
}

#[test]
fn function_with_struct_param_jni() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![Function {
            name: "save".to_string(),
            params: vec![Param {
                name: "contact".to_string(),
                ty: TypeRef::Record("Contact".into()),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("@JvmStatic private external fun saveJni(contact: ByteArray)"),
        "the JNI external must take the packed ByteArray: {kt}"
    );
    assert!(
        kt.contains(
            "fun save(contact: Contact) { saveJni(weaveEncode { w -> packContact(w, contact) }) }"
        ),
        "the wrapper must pack the record before crossing: {kt}"
    );

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains(
            "weaveffi_contacts_save((const uint8_t*)contact_elems, (size_t)contact_len, &err)"
        ),
        "the buffered param must cross as borrowed (ptr, len): {jni}"
    );
}

#[test]
fn function_returning_struct_jni() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![Function {
            name: "create".to_string(),
            params: vec![Param {
                name: "age".to_string(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_contacts_create((int32_t)age, &out_len, &err);"),
        "buffered record return must thread out_len: {jni}"
    );
    assert!(
        jni.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
        "buffered record return must copy into a ByteArray: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
        "the producer allocation must be freed: {jni}"
    );
}

// --- Enum tests ---

#[test]
fn kotlin_enum_class_generated() {
    let api = make_api(vec![Module {
        name: "paint".to_string(),
        functions: vec![],
        structs: vec![],
        enums: vec![EnumDef {
            name: "Color".to_string(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".to_string(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".to_string(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Blue".to_string(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("enum class Color(val value: Int) {"),
        "missing enum class: {kt}"
    );
    assert!(kt.contains("Red(0),"), "missing Red variant: {kt}");
    assert!(kt.contains("Green(1),"), "missing Green variant: {kt}");
    assert!(
        kt.contains("Blue(2);"),
        "missing Blue variant (with semicolon): {kt}"
    );
    assert!(
        kt.contains("companion object {"),
        "missing companion object: {kt}"
    );
    assert!(
        kt.contains("fun fromValue(value: Int): Color"),
        "missing fromValue: {kt}"
    );
}

#[test]
fn kotlin_type_for_enum_returns_name() {
    assert_eq!(kotlin_type(&TypeRef::Enum("Color".into())), "Color");
}

#[test]
fn kotlin_jni_type_for_enum_is_int() {
    assert_eq!(kotlin_jni_type(&TypeRef::Enum("Color".into())), "Int");
}

#[test]
fn function_with_enum_param_kotlin() {
    let api = make_api(vec![Module {
        name: "paint".to_string(),
        functions: vec![Function {
            name: "set_color".to_string(),
            params: vec![Param {
                name: "color".to_string(),
                ty: TypeRef::Enum("Color".into()),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("color: Color"),
        "public wrapper should use enum class name: {kt}"
    );
    assert!(
        kt.contains("private external fun setColorJni(color: Int)"),
        "native function should use Int for JNI: {kt}"
    );
    assert!(
        kt.contains("color.value"),
        "wrapper should call .value on enum param: {kt}"
    );
}

#[test]
fn kotlin_function_uses_enum_type() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![Function {
            name: "add_contact".to_string(),
            params: vec![
                Param {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "contact_type".to_string(),
                    ty: TypeRef::Enum("ContactType".into()),
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::Enum("ContactType".into())),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("contactType: ContactType"),
        "public signature should use enum class name, not Int: {kt}"
    );
    assert!(
        kt.contains("): ContactType"),
        "return type should use enum class name: {kt}"
    );
    assert!(
        !kt.contains("external fun addContact("),
        "public function should not be external: {kt}"
    );
    assert!(
        kt.contains("private external fun addContactJni("),
        "native function should be private: {kt}"
    );
    assert!(
        kt.contains("contactType.value"),
        "wrapper should extract int via .value: {kt}"
    );
    assert!(
        kt.contains("ContactType.fromValue("),
        "wrapper should wrap return in fromValue: {kt}"
    );
}

#[test]
fn function_with_enum_param_jni() {
    let api = make_api(vec![Module {
        name: "paint".to_string(),
        functions: vec![Function {
            name: "set_color".to_string(),
            params: vec![Param {
                name: "color".to_string(),
                ty: TypeRef::Enum("Color".into()),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jint color"),
        "missing jint param in JNI: {jni}"
    );
    assert!(
        jni.contains("(int32_t)color"),
        "missing int32_t cast: {jni}"
    );
    assert!(
        jni.contains("WeaveFFI_setColorJni("),
        "JNI function name should carry the camelCase Jni suffix: {jni}"
    );
}

#[test]
fn function_returning_enum_jni() {
    let api = make_api(vec![Module {
        name: "paint".to_string(),
        functions: vec![Function {
            name: "get_color".to_string(),
            params: vec![],
            returns: Some(TypeRef::Enum("Color".into())),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("JNIEXPORT jint JNICALL"),
        "missing jint return in JNI: {jni}"
    );
    assert!(jni.contains("(jint)"), "missing jint cast: {jni}");
    assert!(
        jni.contains("WeaveFFI_getColorJni("),
        "JNI function name should carry the camelCase Jni suffix: {jni}"
    );
}

// --- Optional tests ---

#[test]
fn kotlin_type_for_optional_int() {
    assert_eq!(
        kotlin_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "Int?"
    );
}

#[test]
fn kotlin_type_for_optional_string() {
    assert_eq!(
        kotlin_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        "String?"
    );
}

#[test]
fn function_with_optional_int_param_kotlin() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "find".to_string(),
            params: vec![Param {
                name: "id".to_string(),
                ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(kt.contains("id: Int?"), "missing optional Int? param: {kt}");
}

#[test]
fn function_with_optional_int_param_jni() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "find".to_string(),
            params: vec![Param {
                name: "id".to_string(),
                ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jbyteArray id"),
        "optional param must cross as a packed jbyteArray: {jni}"
    );
    assert!(
        jni.contains("weaveffi_store_find((const uint8_t*)id_elems, (size_t)id_len, &err)"),
        "optional param must pass borrowed (ptr, len): {jni}"
    );
    assert!(
        jni.contains("(*env)->ReleaseByteArrayElements(env, id, id_elems, JNI_ABORT);"),
        "the pinned encoding must be released without copy-back: {jni}"
    );
}

#[test]
fn function_with_optional_string_param_jni() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "find_name".to_string(),
            params: vec![Param {
                name: "query".to_string(),
                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jbyteArray query"),
        "optional string param must cross as a packed jbyteArray: {jni}"
    );
    assert!(
        jni.contains(
            "weaveffi_store_find_name((const uint8_t*)query_elems, (size_t)query_len, &err)"
        ),
        "optional string param must pass borrowed (ptr, len): {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("weaveEncode { w -> w.writeOptional(query) { v0 -> w.writeString(v0) } }"),
        "the wrapper must write the flag byte plus string: {kt}"
    );
}

#[test]
fn function_returning_optional_int_jni() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "lookup".to_string(),
            params: vec![],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("JNIEXPORT jbyteArray JNICALL"),
        "optional return must cross as a value buffer: {jni}"
    );
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_store_lookup(&out_len, &err);"),
        "optional return must thread out_len: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains(
            "fun lookup(): Int? = weaveDecode(lookupJni()) { r -> r.readOptional { r.readI32() } }"
        ),
        "the wrapper must decode the optional value: {kt}"
    );
}

#[test]
fn function_returning_optional_string_jni() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "get_name".to_string(),
            params: vec![],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_store_get_name(&out_len, &err);"),
        "optional string return crosses as a value buffer: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun getName(): String? = weaveDecode(getNameJni()) { r -> r.readOptional { r.readString() } }"),
        "the wrapper must decode the optional string: {kt}"
    );
}

// --- List tests ---

#[test]
fn kotlin_type_for_list_int() {
    assert_eq!(
        kotlin_type(&TypeRef::List(Box::new(TypeRef::I32))),
        "List<Int>"
    );
}

#[test]
fn kotlin_type_for_list_string() {
    assert_eq!(
        kotlin_type(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
        "List<String>"
    );
}

#[test]
fn kotlin_type_for_list_enum() {
    assert_eq!(
        kotlin_type(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
        "List<Color>"
    );
}

#[test]
fn function_with_list_int_param_kotlin() {
    let api = make_api(vec![Module {
        name: "batch".to_string(),
        functions: vec![Function {
            name: "process".to_string(),
            params: vec![Param {
                name: "ids".to_string(),
                ty: TypeRef::List(Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun process(ids: List<Int>)"),
        "the public wrapper takes an idiomatic List<Int>: {kt}"
    );
    assert!(
        kt.contains("processJni(weaveEncode { w -> w.writeList(ids) { v0 -> w.writeI32(v0) } })"),
        "the wrapper must pack the list into a value buffer: {kt}"
    );
}

#[test]
fn function_with_list_int_param_jni() {
    let api = make_api(vec![Module {
        name: "batch".to_string(),
        functions: vec![Function {
            name: "process".to_string(),
            params: vec![Param {
                name: "ids".to_string(),
                ty: TypeRef::List(Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jbyteArray ids"),
        "list param must cross as a packed jbyteArray: {jni}"
    );
    assert!(
        jni.contains("GetByteArrayElements(env, ids, NULL)"),
        "missing byte-array pin: {jni}"
    );
    assert!(
        jni.contains("ReleaseByteArrayElements(env, ids, ids_elems, JNI_ABORT)"),
        "missing byte-array release: {jni}"
    );
    assert!(
        jni.contains("weaveffi_batch_process((const uint8_t*)ids_elems, (size_t)ids_len, &err)"),
        "list param must pass borrowed (ptr, len): {jni}"
    );
}

#[test]
fn function_returning_list_int_jni() {
    let api = make_api(vec![Module {
        name: "batch".to_string(),
        functions: vec![Function {
            name: "get_ids".to_string(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("JNIEXPORT jbyteArray JNICALL"),
        "list return must cross as a value buffer: {jni}"
    );
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_batch_get_ids(&out_len, &err);"),
        "list return must thread out_len: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun getIds(): List<Int> = weaveDecode(getIdsJni()) { r -> r.readList { r.readI32() } }"),
        "the wrapper must decode the buffered list: {kt}"
    );
}

#[test]
fn jni_param_type_enum_is_jint() {
    assert_eq!(jni_param_type(&TypeRef::Enum("Color".into())), "jint");
}

#[test]
fn jni_param_type_optional_int_is_buffered() {
    assert_eq!(
        jni_param_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "jbyteArray"
    );
}

#[test]
fn jni_param_type_optional_string_is_buffered() {
    assert_eq!(
        jni_param_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        "jbyteArray"
    );
}

#[test]
fn jni_param_type_optional_interface_is_nullable_pointer() {
    assert_eq!(
        jni_param_type(&TypeRef::Optional(Box::new(TypeRef::Interface(
            "Store".into()
        )))),
        "jobject"
    );
}

#[test]
fn jni_param_type_list_int_is_buffered() {
    assert_eq!(
        jni_param_type(&TypeRef::List(Box::new(TypeRef::I32))),
        "jbyteArray"
    );
}

#[test]
fn jni_param_type_list_long_is_buffered() {
    assert_eq!(
        jni_param_type(&TypeRef::List(Box::new(TypeRef::I64))),
        "jbyteArray"
    );
}

#[test]
fn generate_android_with_structs_and_enums() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![Function {
            name: "get_contact".to_string(),
            params: vec![Param {
                name: "id".to_string(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![
                StructField {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "email".to_string(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
                    doc: None,
                },
            ],
        }],
        enums: vec![EnumDef {
            name: "Color".to_string(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".to_string(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".to_string(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Blue".to_string(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_android_structs_and_enums");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    AndroidGenerator
        .generate(&api, out_dir, &AndroidConfig::default())
        .unwrap();

    let kotlin =
        std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .unwrap();

    assert!(
        kotlin.contains("enum class Color(val value: Int) {"),
        "missing enum class: {kotlin}"
    );
    assert!(kotlin.contains("Red(0),"), "missing Red variant: {kotlin}");
    assert!(
        kotlin.contains("Green(1),"),
        "missing Green variant: {kotlin}"
    );
    assert!(
        kotlin.contains("Blue(2);"),
        "missing Blue variant with semicolon: {kotlin}"
    );
    assert!(
        kotlin.contains("fun fromValue(value: Int): Color"),
        "missing fromValue: {kotlin}"
    );

    assert!(
        kotlin.contains("data class Contact(val name: String, val email: String, val age: Int)"),
        "record must be a data class with typed properties: {kotlin}"
    );
    assert!(
        kotlin.contains("internal fun packContact(")
            && kotlin.contains("internal fun unpackContact("),
        "record codecs missing: {kotlin}"
    );
    assert!(
        !kotlin.contains("nativeCreate") && !kotlin.contains("nativeGet"),
        "records must have no native surface: {kotlin}"
    );
    assert!(
        kotlin.contains(
            "fun getContact(id: Int): Contact = weaveDecode(getContactJni(id)) { r -> unpackContact(r) }"
        ),
        "the wrapper must decode the buffered record return: {kotlin}"
    );

    let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

    assert!(
        jni.contains(
            "const uint8_t* rv = weaveffi_contacts_get_contact((int32_t)id, &out_len, &err);"
        ),
        "buffered record return must thread out_len: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
        "the producer allocation must be freed after copying: {jni}"
    );
    assert!(
        !jni.contains("weaveffi_contacts_Contact_"),
        "records must expose no per-field C symbols: {jni}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn kotlin_type_for_map() {
    assert_eq!(
        kotlin_type(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "Map<String, Int>"
    );
    assert_eq!(
        kotlin_type(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::F64)
        )),
        "Map<String, Double>"
    );
    assert_eq!(
        kotlin_type(&TypeRef::Map(
            Box::new(TypeRef::I32),
            Box::new(TypeRef::StringUtf8)
        )),
        "Map<Int, String>"
    );
}

#[test]
fn function_with_map_param_kotlin() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "update_scores".to_string(),
            params: vec![Param {
                name: "scores".to_string(),
                ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("scores: Map<String, Int>"),
        "missing Map<String, Int> param: {kt}"
    );
}

#[test]
fn function_with_map_param_jni() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "update_scores".to_string(),
            params: vec![Param {
                name: "scores".to_string(),
                ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("jbyteArray scores"),
        "map param must cross as a packed jbyteArray: {jni}"
    );
    assert!(
        jni.contains("GetByteArrayElements(env, scores, NULL)"),
        "missing byte-array pin: {jni}"
    );
    assert!(
        jni.contains(
            "weaveffi_store_update_scores((const uint8_t*)scores_elems, (size_t)scores_len, &err)"
        ),
        "map param must pass borrowed (ptr, len): {jni}"
    );
    assert!(
        jni.contains("ReleaseByteArrayElements(env, scores, scores_elems, JNI_ABORT)"),
        "missing byte-array release: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("weaveEncode { w -> w.writeMap(scores, { k0 -> w.writeString(k0) }, { v0 -> w.writeI32(v0) }) }"),
        "the wrapper must pack the map before crossing: {kt}"
    );
}

#[test]
fn android_build_gradle_has_cmake_config() {
    let api = make_api(vec![Module {
        name: "math".to_string(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_android_build_gradle_cmake");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    AndroidGenerator
        .generate(&api, out_dir, &AndroidConfig::default())
        .unwrap();

    let gradle = std::fs::read_to_string(tmp.join("android/build.gradle")).unwrap();
    assert!(
        gradle.contains("externalNativeBuild"),
        "missing externalNativeBuild in build.gradle: {gradle}"
    );
    assert!(
        gradle.contains("path \"src/main/cpp/CMakeLists.txt\""),
        "missing cmake path in build.gradle: {gradle}"
    );
    assert!(
        gradle.contains("cppFlags \"\""),
        "missing cppFlags in build.gradle: {gradle}"
    );
    assert!(
        gradle.contains("namespace 'com.weaveffi'"),
        "missing namespace in build.gradle: {gradle}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn function_returning_map_jni() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![Function {
            name: "get_scores".to_string(),
            params: vec![],
            returns: Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("JNIEXPORT jbyteArray JNICALL"),
        "map return must cross as a value buffer: {jni}"
    );
    assert!(
        jni.contains("const uint8_t* rv = weaveffi_store_get_scores(&out_len, &err);"),
        "map return must thread out_len: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
        "the producer allocation must be freed after copying: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains(
            "fun getScores(): Map<String, Int> = weaveDecode(getScoresJni()) { r -> r.readMap({ r.readString() }, { r.readI32() }) }"
        ),
        "the wrapper must decode the buffered map: {kt}"
    );
}

#[test]
fn android_custom_package() {
    let api = make_api(vec![Module {
        name: "math".to_string(),
        functions: vec![Function {
            name: "add".to_string(),
            params: vec![
                Param {
                    name: "a".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let config = AndroidConfig {
        package: Some("com.mycompany.ffi".into()),
        ..AndroidConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_android_custom_package");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    AndroidGenerator.generate(&api, out_dir, &config).unwrap();

    let kotlin_path = tmp.join("android/src/main/kotlin/com/mycompany/ffi/WeaveFFI.kt");
    assert!(
        kotlin_path.exists(),
        "Kotlin file not at custom package path"
    );

    let kotlin = std::fs::read_to_string(&kotlin_path).unwrap();
    assert!(
        kotlin.contains("package com.mycompany.ffi"),
        "missing custom package declaration: {kotlin}"
    );
    assert!(
        !kotlin.contains("package com.weaveffi"),
        "should not contain default package: {kotlin}"
    );

    let gradle = std::fs::read_to_string(tmp.join("android/build.gradle")).unwrap();
    assert!(
        gradle.contains("namespace 'com.mycompany.ffi'"),
        "missing custom namespace in build.gradle: {gradle}"
    );

    let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
    assert!(
        jni.contains("Java_com_mycompany_ffi_WeaveFFI_add"),
        "missing custom JNI prefix: {jni}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// One module declaring an error domain, with one throwing and one
/// non-throwing function, shared by the typed-error tests.
fn make_error_api() -> ResolvedApi {
    make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![
            Function {
                name: "get".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                throws: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "count".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "ContactError".to_string(),
            codes: vec![
                ErrorCode {
                    name: "ContactNotFound".to_string(),
                    code: 1001,
                    message: "Contact not found".to_string(),
                    doc: None,
                    fields: vec![],
                },
                ErrorCode {
                    name: "InvalidInput".to_string(),
                    code: 1002,
                    message: "Invalid input provided".to_string(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }])
}

#[test]
fn kotlin_inline_error_types() {
    let kt = render_kotlin(&make_error_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains(
            "open class WeaveFFIException(val code: Int, message: String) : Exception(message)"
        ),
        "missing open generic exception: {kt}"
    );
    assert!(
        kt.contains("sealed class ContactException(code: Int, message: String) : WeaveFFIException(code, message) {"),
        "missing sealed domain exception: {kt}"
    );
    assert!(
        kt.contains("class ContactNotFound(message: String = \"Contact not found\") : ContactException(1001, message)"),
        "missing ContactNotFound subclass: {kt}"
    );
    assert!(
        kt.contains("class InvalidInput(message: String = \"Invalid input provided\") : ContactException(1002, message)"),
        "missing InvalidInput subclass: {kt}"
    );
    assert!(
        kt.contains(
            "fun fromCode(code: Int, message: String, payload: ByteArray?): WeaveFFIException = when (code) {"
        ),
        "missing fromCode factory: {kt}"
    );
    assert!(
        kt.contains("1001 -> ContactNotFound(message)"),
        "fromCode must map 1001: {kt}"
    );
    assert!(
        kt.contains("else -> WeaveFFIException(code, message)"),
        "fromCode must fall back to the generic exception: {kt}"
    );
}

#[test]
fn kotlin_error_payload_fields_decode() {
    let mut api = make_error_api().api().clone();
    api.modules[0].errors.as_mut().unwrap().codes[0].fields = vec![
        field("contact_id", TypeRef::I64),
        field("hint", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
    ];
    let api = ResolvedApi::assume_resolved(api);
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains(
            "class ContactNotFound(message: String = \"Contact not found\", val contact_id: Long, val hint: String?) : ContactException(1001, message)"
        ),
        "payload fields must be constructor properties: {kt}"
    );
    assert!(
        kt.contains(
            "1001 -> if (payload != null) weaveDecode(payload) { r -> ContactNotFound(message, r.readI64(), r.readOptional { r.readString() }) } else WeaveFFIException(code, message)"
        ),
        "fromCode must decode the payload in declaration order: {kt}"
    );
}

#[test]
fn jni_typed_error_throwers() {
    let jni = render_jni_c(
        &make_error_api(),
        "com.weaveffi",
        true,
        "weaveffi.yml",
        "weaveffi",
    );
    // The generic thrower constructs the brand exception with (code, message).
    assert!(
        jni.contains("static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {"),
        "missing generic thrower: {jni}"
    );
    assert!(
        jni.contains("FindClass(env, \"com/weaveffi/WeaveFFIException\")"),
        "generic thrower must construct the brand exception: {jni}"
    );
    assert!(
        jni.contains("\"<init>\", \"(ILjava/lang/String;)V\""),
        "generic thrower must pass the raw code: {jni}"
    );
    // The domain thrower maps known codes to typed subclasses.
    assert!(
        jni.contains(
            "static void throw_weaveffi_contacts_ContactError(JNIEnv* env, weaveffi_error* err) {"
        ),
        "missing domain thrower: {jni}"
    );
    assert!(
        jni.contains(
            "GetStaticMethodID(env, exClass, \"fromCode\", \"(ILjava/lang/String;[B)Lcom/weaveffi/WeaveFFIException;\")"
        ),
        "domain thrower must resolve the fromCode factory: {jni}"
    );
    assert!(
        jni.contains("SetByteArrayRegion(env, jpayload, 0, (jsize)err->payload_len, (const jbyte*)err->payload_ptr)"),
        "domain thrower must copy the payload into a jbyteArray: {jni}"
    );
    assert!(
        jni.contains(
            "CallStaticObjectMethod(env, exClass, fromCode, (jint)err->code, jmsg, jpayload)"
        ),
        "domain thrower must dispatch through fromCode: {jni}"
    );
    assert!(
        jni.contains("throw_weaveffi_error(env, err);"),
        "an unresolvable factory must fall back to the generic thrower: {jni}"
    );
    assert!(
        jni.contains("weaveffi_error_clear(err);"),
        "the thrower must release the message and payload: {jni}"
    );
}

#[test]
fn jni_throws_split_picks_thrower_per_function() {
    let jni = render_jni_c(
        &make_error_api(),
        "com.weaveffi",
        true,
        "weaveffi.yml",
        "weaveffi",
    );
    let get_body = jni
        .split("Java_com_weaveffi_WeaveFFI_get(")
        .nth(1)
        .expect("get export");
    let get_body = &get_body[..get_body.find("\nJNIEXPORT").unwrap_or(get_body.len())];
    assert!(
        get_body.contains("throw_weaveffi_contacts_ContactError(env, &err);"),
        "throwing function must dispatch to the domain thrower: {jni}"
    );
    let count_body = jni
        .split("Java_com_weaveffi_WeaveFFI_count(")
        .nth(1)
        .expect("count export");
    let count_body = &count_body[..count_body.find("\nJNIEXPORT").unwrap_or(count_body.len())];
    assert!(
        count_body.contains("throw_weaveffi_error(env, &err);"),
        "non-throwing function must dispatch to the generic thrower: {jni}"
    );
    assert!(
        !count_body.contains("throw_weaveffi_contacts_ContactError"),
        "non-throwing function must not use the domain thrower: {jni}"
    );
}

#[test]
fn android_strip_module_prefix() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![Function {
            name: "create_contact".to_string(),
            params: vec![Param {
                name: "name".to_string(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    // Stripping is the default: the config's `Default` must strip, and the
    // emitted Kotlin name is the bare lowerCamelCase function name.
    let config = AndroidConfig::default();
    assert!(
        config.strip_module_prefix,
        "strip_module_prefix must default to true"
    );

    let tmp = std::env::temp_dir().join("weaveffi_test_android_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    AndroidGenerator.generate(&api, out_dir, &config).unwrap();

    let kotlin =
        std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .unwrap();

    assert!(
        kotlin.contains("fun createContact("),
        "stripped name should be createContact: {kotlin}"
    );
    assert!(
        !kotlin.contains("fun contactsCreateContact("),
        "should not contain module-prefixed name: {kotlin}"
    );

    let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

    assert!(
        jni.contains("weaveffi_contacts_create_contact"),
        "C ABI call should still use full name: {jni}"
    );

    let no_strip = AndroidConfig {
        strip_module_prefix: false,
        ..AndroidConfig::default()
    };
    let tmp2 = std::env::temp_dir().join("weaveffi_test_android_no_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp2);
    std::fs::create_dir_all(&tmp2).unwrap();
    let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

    AndroidGenerator
        .generate(&api, out_dir2, &no_strip)
        .unwrap();

    let kotlin2 =
        std::fs::read_to_string(tmp2.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .unwrap();

    assert!(
        kotlin2.contains("fun contactsCreateContact("),
        "opting out must keep the module-prefixed name: {kotlin2}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&tmp2);
}

#[test]
fn android_deeply_nested_optional() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "data".into(),
                ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                    Box::new(TypeRef::Record("Contact".into())),
                ))))),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kotlin.contains("data: List<Contact?>?"),
        "should contain deeply nested optional type: {kotlin}"
    );
    assert!(
        kotlin.contains(
            "w.writeOptional(data) { v0 -> w.writeList(v0) { v1 -> w.writeOptional(v1) { v2 -> packContact(w, v2) } } }"
        ),
        "nested optionals must pack recursively: {kotlin}"
    );
}

#[test]
fn android_map_of_lists() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "scores".into(),
                ty: TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                ),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kotlin.contains("scores: Map<String, List<Int>>"),
        "should contain map of lists type: {kotlin}"
    );
}

#[test]
fn android_enum_keyed_map() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "contacts".into(),
                ty: TypeRef::Map(
                    Box::new(TypeRef::Enum("Color".into())),
                    Box::new(TypeRef::Record("Contact".into())),
                ),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Blue".into(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kotlin.contains("contacts: Map<Color, Contact>"),
        "should contain enum-keyed map type: {kotlin}"
    );
    assert!(
        kotlin.contains(
            "w.writeMap(contacts, { k0 -> w.writeI32(k0.value) }, { v0 -> packContact(w, v0) })"
        ),
        "enum keys pack as their raw value; record values recurse: {kotlin}"
    );
}

#[test]
fn android_typed_handle_type() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "get_info".into(),
            params: vec![Param {
                name: "contact".into(),
                ty: TypeRef::TypedHandle("Contact".into()),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("contact: Long"),
        "TypedHandle is an opaque u64 token surfacing as Long: {kt}"
    );
}

#[test]
fn android_no_double_free_on_error() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "find_contact".into(),
            params: vec![Param {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains("GetStringUTFChars"),
        "input StringUtf8 should use GetStringUTFChars: {jni}"
    );
    assert!(
        jni.contains("ReleaseStringUTFChars"),
        "input StringUtf8 should release JVM chars: {jni}"
    );
    assert!(
        !jni.contains("weaveffi_free_string(name"),
        "input string param must not be freed via WeaveFFI: {jni}"
    );

    let start = jni
        .find("Java_com_weaveffi_WeaveFFI_findContactJni")
        .expect("find_contact JNI symbol");
    let rest = &jni[start..];
    let end = rest.find("\nJNIEXPORT ").unwrap_or(rest.len());
    let fn_body = &rest[..end];
    let release_pos = fn_body
        .find("ReleaseStringUTFChars")
        .expect("borrowed param released after the call");
    let err_pos = fn_body
        .find("if (err.code != 0)")
        .expect("error check before using return value");
    let free_pos = fn_body
        .find("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);")
        .expect("buffered return freed after copying");
    assert!(
        release_pos < err_pos && err_pos < free_pos,
        "release, then err check, then copy-and-free; the error path must not free: {jni}"
    );
    assert!(
        fn_body.contains("throw_weaveffi_error"),
        "error path should throw: {jni}"
    );

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("data class Contact"),
        "record data class Contact: {kt}"
    );
}

#[test]
fn android_custom_prefix_threads_to_c_symbols() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "greet".into(),
            params: vec![Param {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "myffi");

    // The JNI C shim must call the user C symbol with the custom C ABI
    // prefix, and include the matching C header `myffi.h`.
    assert!(
        jni.contains("myffi_contacts_greet("),
        "shim should call custom-prefixed user C symbol: {jni}"
    );
    assert!(
        jni.contains("#include \"myffi.h\""),
        "shim should include the custom C header: {jni}"
    );
    // The default-prefixed user C symbol must NOT leak into the shim.
    assert!(
        !jni.contains("weaveffi_contacts_greet"),
        "default-prefixed user C symbol must not appear: {jni}"
    );
    // JNI export names are package-derived (not C-ABI-prefixed) and stay
    // literal regardless of the C ABI prefix.
    assert!(
        jni.contains("Java_com_weaveffi_WeaveFFI_greet"),
        "JNI export name must stay package-derived: {jni}"
    );
    // Runtime helpers keep the literal `weaveffi_` runtime prefix.
    assert!(
        jni.contains("weaveffi_error"),
        "runtime weaveffi_error helper must remain literal: {jni}"
    );
    assert!(
        jni.contains("weaveffi_free_string"),
        "runtime weaveffi_free_string helper must remain literal: {jni}"
    );
}

#[test]
fn android_null_check_on_optional_return() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "find_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    assert!(
        jni.contains(
            "const uint8_t* rv = weaveffi_contacts_find_contact((int32_t)id, &out_len, &err);"
        ),
        "optional record return must cross as a value buffer: {jni}"
    );
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains(
            "fun findContact(id: Int): Contact? = weaveDecode(findContactJni(id)) { r -> r.readOptional { unpackContact(r) } }"
        ),
        "the wrapper must decode the optional flag byte: {kt}"
    );
}

#[test]
fn kotlin_async_function_is_suspend() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "run".to_string(),
            params: vec![Param {
                name: "id".to_string(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("suspend fun"),
        "async function should generate suspend fun: {kt}"
    );
    assert!(
        kt.contains("suspend fun run(id: Int): Int"),
        "suspend fun should have correct signature: {kt}"
    );
}

#[test]
fn kotlin_async_uses_coroutine() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "run".to_string(),
            params: vec![Param {
                name: "id".to_string(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("suspendCancellableCoroutine"),
        "async function should use suspendCancellableCoroutine: {kt}"
    );
    assert!(
        kt.contains("WeaveContinuation"),
        "async function should use WeaveContinuation: {kt}"
    );
    assert!(
        kt.contains("import kotlinx.coroutines.suspendCancellableCoroutine"),
        "should import suspendCancellableCoroutine: {kt}"
    );
}

/// JNI requires `NewGlobalRef` on the Kotlin continuation so it survives
/// across the C-side thread spawn, balanced by `DeleteGlobalRef` in the
/// JNI callback after the suspend point is resumed. The `malloc` of the
/// callback context must also be balanced by `free(ctx)`.
#[test]
fn android_async_pins_callback_for_lifetime() {
    let api = make_api(vec![Module {
        name: "tasks".into(),
        functions: vec![Function {
            name: "run".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let c = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    let pin_count = c.matches("NewGlobalRef(env, callback)").count();
    let unpin_count = c.matches("DeleteGlobalRef(env, ctx->callback)").count();
    let malloc_count = c.matches("malloc(sizeof(weaveffi_jni_async_ctx))").count();
    let free_count = c.matches("free(ctx);").count();
    assert_eq!(
        pin_count, 1,
        "expected one NewGlobalRef per async fn, got {pin_count}: {c}"
    );
    assert_eq!(
        unpin_count, 1,
        "expected one DeleteGlobalRef per async fn, got {unpin_count}: {c}"
    );
    // One allocation; two textual frees because the attach-failure early
    // return must also release the context (each runtime path frees once).
    assert_eq!(
        malloc_count, 1,
        "expected one ctx malloc per async fn, got {malloc_count}: {c}"
    );
    assert_eq!(
        free_count, 2,
        "expected a free on both the completion and attach-failure paths, got {free_count}: {c}"
    );
    // The producer thread must not stay attached after completion.
    assert!(
        c.contains("DetachCurrentThread"),
        "async completion must detach the producer thread: {c}"
    );
}

fn doc_api() -> ResolvedApi {
    make_api(vec![Module {
        name: "docs".into(),
        functions: vec![Function {
            name: "do_thing".into(),
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: Some("the input value".into()),
            }],
            returns: Some(TypeRef::I32),
            doc: Some("Performs a thing.".into()),
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: Some("An item we track.".into()),
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: Some("Stable id".into()),
            }],
        }],
        enums: vec![EnumDef {
            name: "Kind".into(),
            doc: Some("Kind of item.".into()),
            variants: vec![EnumVariant {
                name: "Small".into(),
                value: 0,
                doc: Some("A small one".into()),
                fields: vec![],
            }],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "DocsErrors".into(),
            codes: vec![ErrorCode {
                name: "not_found".into(),
                code: 1,
                message: "Not found".into(),
                doc: Some("Raised when missing".into()),
                fields: vec![],
            }],
        }),
        modules: vec![],
    }])
}

#[test]
fn android_emits_doc_on_function() {
    let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(kt.contains("Performs a thing."), "{kt}");
}

#[test]
fn android_emits_doc_on_struct() {
    let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(kt.contains("/** An item we track. */"), "{kt}");
}

#[test]
fn android_emits_doc_on_enum_variant() {
    let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(kt.contains("/** Kind of item. */"), "{kt}");
    assert!(kt.contains("/** A small one */"), "{kt}");
}

#[test]
fn android_emits_doc_on_field() {
    let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(kt.contains("/** Stable id */"), "{kt}");
}

#[test]
fn android_emits_doc_on_param() {
    let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(kt.contains("@param x the input value"), "{kt}");
}

/// A `kv` module with a `Store` interface exercising every member shape:
/// the `new` constructor, a named factory, sync methods (throwing and
/// not), an async throwing method, a static, and an interface-typed
/// parameter and return.
fn make_interface_api() -> ResolvedApi {
    use weaveffi_ir::ir::InterfaceDef;
    make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![Function {
            name: "merge".to_string(),
            params: vec![
                Param {
                    name: "left_store".to_string(),
                    ty: TypeRef::Interface("Store".to_string()),
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "right_store".to_string(),
                    ty: TypeRef::Interface("Store".to_string()),
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::Interface("Store".to_string())),
            doc: None,
            r#async: false,
            throws: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![InterfaceDef {
            name: "Store".to_string(),
            doc: Some("A key-value store.".to_string()),
            constructors: vec![
                Function {
                    name: "new".to_string(),
                    params: vec![Param {
                        name: "path".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "open_readonly".to_string(),
                    params: vec![Param {
                        name: "path".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    throws: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            methods: vec![
                Function {
                    name: "get".to_string(),
                    params: vec![Param {
                        name: "key".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    throws: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "len".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::U64),
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "fetch".to_string(),
                    params: vec![Param {
                        name: "key".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: true,
                    throws: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            statics: vec![Function {
                name: "default_path".to_string(),
                params: vec![],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        }],
        errors: Some(ErrorDomain {
            name: "KvError".to_string(),
            codes: vec![ErrorCode {
                name: "KeyNotFound".to_string(),
                code: 100,
                message: "Key not found".to_string(),
                doc: None,
                fields: vec![],
            }],
        }),
        modules: vec![],
    }])
}

#[test]
fn kotlin_interface_class_shape() {
    let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains(
            "class Store internal constructor(internal var handle: Long) : java.io.Closeable {"
        ),
        "missing handle-backed Closeable class: {kt}"
    );
    assert!(
        kt.contains("@JvmStatic private external fun nativeDestroy(handle: Long)"),
        "missing destroy external: {kt}"
    );
    assert!(
        kt.contains("override fun close() {") && kt.contains("nativeDestroy(handle)"),
        "close() must call the destroy symbol: {kt}"
    );
    assert!(
        kt.contains("protected fun finalize() {"),
        "missing finalizer safety net: {kt}"
    );
}

#[test]
fn kotlin_interface_constructors_and_statics() {
    let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("operator fun invoke(path: String): Store = Store(nativeNew(path))"),
        "the new constructor must become operator fun invoke: {kt}"
    );
    assert!(
        kt.contains("fun openReadonly(path: String): Store = Store(nativeOpenReadonly(path))"),
        "named constructors must become companion factories: {kt}"
    );
    assert!(
        kt.contains("fun defaultPath(): String = nativeDefaultPath()"),
        "statics must become companion functions: {kt}"
    );
}

#[test]
fn kotlin_interface_methods() {
    let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
    assert!(
        kt.contains("fun get(key: String): String = nativeGet(handle, key)"),
        "methods must pass the handle as the leading native argument: {kt}"
    );
    assert!(
        kt.contains("suspend fun fetch(key: String): String = suspendCancellableCoroutine"),
        "async methods must be suspend funs: {kt}"
    );
    assert!(
        kt.contains(
            "nativeFetchAsync(handle, key, WeaveContinuation(cont) { code, message, payload -> KvException.fromCode(code, message, payload) })"
        ),
        "async throwing methods must map errors through the typed domain: {kt}"
    );
}

#[test]
fn kotlin_interface_params_and_returns() {
    let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
    // Interface-typed parameters accept the class and pass the raw handle;
    // interface returns re-wrap the owned pointer. Parameter names are
    // camelCased from the IR's snake_case.
    assert!(
        kt.contains(
            "@JvmStatic fun merge(leftStore: Store, rightStore: Store): Store = Store(mergeJni(leftStore.handle, rightStore.handle))"
        ),
        "interface params must unwrap handles and returns must re-wrap: {kt}"
    );
}

#[test]
fn jni_interface_bridge_members() {
    let jni = render_jni_c(
        &make_interface_api(),
        "com.weaveffi",
        true,
        "weaveffi.yml",
        "weaveffi",
    );
    assert!(
        jni.contains("JNIEXPORT jlong JNICALL Java_com_weaveffi_Store_nativeNew(JNIEnv* env, jclass clazz, jstring path)"),
        "missing constructor export: {jni}"
    );
    assert!(
        jni.contains("weaveffi_kv_Store_new(path_chars, &err)"),
        "constructor must call the lowered ABI symbol: {jni}"
    );
    assert!(
        jni.contains("JNIEXPORT jstring JNICALL Java_com_weaveffi_Store_nativeGet(JNIEnv* env, jclass clazz, jlong selfHandle, jstring key)"),
        "missing method export with leading self slot: {jni}"
    );
    assert!(
        jni.contains(
            "weaveffi_kv_Store_get((const weaveffi_kv_Store*)(intptr_t)selfHandle, key_chars, &err)"
        ),
        "method must pass the receiver as the leading ABI argument: {jni}"
    );
    assert!(
        jni.contains("weaveffi_kv_Store_default_path(&err)"),
        "static must call its ABI symbol: {jni}"
    );
    assert!(
        jni.contains("JNIEXPORT void JNICALL Java_com_weaveffi_Store_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle)")
            && jni.contains("weaveffi_kv_Store_destroy((weaveffi_kv_Store*)(intptr_t)handle);"),
        "missing destroy export: {jni}"
    );
    assert!(
        jni.contains("JNIEXPORT void JNICALL Java_com_weaveffi_Store_nativeFetchAsync"),
        "missing async method launcher: {jni}"
    );
    assert!(
        jni.contains(
            "weaveffi_kv_Store_fetch_async((const weaveffi_kv_Store*)(intptr_t)selfHandle, key_chars, weaveffi_kv_Store_fetch_jni_cb, ctx);"
        ),
        "async method must forward the receiver to the ABI launcher: {jni}"
    );
}

/// Generate the Android and C outputs for the shipped sample IDLs through
/// the same parse-validate-generate pipeline the CLI drives, writing into
/// the conformance harness's expected layout
/// (`target/conformance-gen/<sample>/{android,c}`). Serves two purposes:
/// it smoke-tests generation against the real sample surfaces (interfaces,
/// typed errors, iterators, listeners, records, rich enums, async), and it
/// lets the
/// Kotlin conformance lanes run when the full CLI is blocked by other
/// in-flight generator crates. Skips silently when the samples are not
/// present (for example in a packaged crate).
#[test]
fn samples_generate_android_and_c_outputs() {
    let root = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let genroot = root.join("target/conformance-gen");
    for sample in ["events", "kvstore", "shapes"] {
        let idl = root.join(format!("samples/{sample}/{sample}.yml"));
        if !idl.as_std_path().exists() {
            return;
        }
        let contents = std::fs::read_to_string(idl.as_std_path()).unwrap();
        let api = weaveffi_ir::parse::parse_api_str(&contents, "yaml")
            .unwrap_or_else(|e| panic!("parse {sample}: {e}"));
        let api = weaveffi_core::validate::validate_api(api, None)
            .unwrap_or_else(|e| panic!("validate {sample}: {e:?}"));
        let out = genroot.join(sample);
        let android_cfg = AndroidConfig {
            input_basename: Some(format!("{sample}.yml")),
            ..AndroidConfig::default()
        };
        AndroidGenerator
            .generate(&api, &out, &android_cfg)
            .unwrap_or_else(|e| panic!("android generate {sample}: {e}"));
        let c_cfg = weaveffi_gen_c::CConfig {
            input_basename: Some(format!("{sample}.yml")),
            ..Default::default()
        };
        weaveffi_gen_c::CGenerator
            .generate(&api, &out, &c_cfg)
            .unwrap_or_else(|e| panic!("c generate {sample}: {e}"));
        assert!(
            out.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt")
                .as_std_path()
                .exists(),
            "missing Kotlin output for {sample}"
        );
        assert!(
            out.join("c/weaveffi.h").as_std_path().exists(),
            "missing C header for {sample}"
        );
    }
}

#[test]
fn jni_interface_throws_split() {
    let jni = render_jni_c(
        &make_interface_api(),
        "com.weaveffi",
        true,
        "weaveffi.yml",
        "weaveffi",
    );
    let get_body = jni
        .split("Java_com_weaveffi_Store_nativeGet(")
        .nth(1)
        .expect("nativeGet export");
    let get_body = &get_body[..get_body.find("\nJNIEXPORT").unwrap_or(get_body.len())];
    assert!(
        get_body.contains("throw_weaveffi_kv_KvError(env, &err);"),
        "throwing method must use the domain thrower: {jni}"
    );
    let len_body = jni
        .split("Java_com_weaveffi_Store_nativeLen(")
        .nth(1)
        .expect("nativeLen export");
    let len_body = &len_body[..len_body.find("\nJNIEXPORT").unwrap_or(len_body.len())];
    assert!(
        len_body.contains("throw_weaveffi_error(env, &err);"),
        "non-throwing method must use the generic thrower: {jni}"
    );
    // Interface params on free functions borrow: the handles are passed
    // as const pointers, never destroyed by the bridge.
    assert!(
        jni.contains("weaveffi_kv_merge((const weaveffi_kv_Store*)(intptr_t)left_store, (const weaveffi_kv_Store*)(intptr_t)right_store, &err)"),
        "interface params must be passed as borrowed const pointers: {jni}"
    );
}

#[test]
fn kotlin_reserved_word_identifiers_are_escaped() {
    let api = make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![Function {
            name: "fun".to_string(),
            params: vec![
                Param {
                    name: "object".to_string(),
                    ty: TypeRef::I64,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "when".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Entry".to_string(),
            doc: None,
            fields: vec![StructField {
                name: "val".to_string(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    // The function name and both parameters are Kotlin hard/soft keywords:
    // each gains the shared trailing underscore instead of emitting code
    // that cannot compile.
    assert!(
        kt.contains("external fun fun_(object_: Long, when_: Int): Int"),
        "reserved-word function and parameter names must be escaped: {kt}"
    );
    // A record field named `val` is escaped in the data class property and
    // in the pack codec's property access.
    assert!(
        kt.contains("data class Entry(val val_: String)"),
        "reserved-word field names must be escaped in data classes: {kt}"
    );
    assert!(
        kt.contains("w.writeString(v.val_)"),
        "reserved-word field names must be escaped in codecs: {kt}"
    );
}

#[test]
fn c_reserved_word_parameters_are_escaped_in_jni() {
    let api = make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![Function {
            name: "store".to_string(),
            params: vec![Param {
                name: "register".to_string(),
                ty: TypeRef::I64,
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            throws: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    // `register` is a C keyword: the JNI shim's parameter declaration and
    // every use gain the trailing underscore (linkage is unaffected; the
    // export name mangles the Kotlin method name, not C parameter names).
    assert!(
        jni.contains("jlong register_)"),
        "C reserved-word parameters must be escaped in the export signature: {jni}"
    );
    assert!(
        jni.contains("weaveffi_kv_store((int64_t)register_, &err);"),
        "C reserved-word parameters must be escaped at the call site: {jni}"
    );
}

#[test]
fn error_mapping_covers_declared_codes_only_and_traps_negatives() {
    let api = make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![
            Function {
                name: "load".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                throws: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "peek".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "KvError".to_string(),
            codes: vec![
                ErrorCode {
                    name: "NotFound".to_string(),
                    code: 1,
                    message: "not found".to_string(),
                    doc: None,
                    fields: vec![],
                },
                ErrorCode {
                    name: "IoFailure".to_string(),
                    code: 7,
                    message: "IO failure".to_string(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }]);
    let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
    // `fromCode` maps exactly the declared (positive-only) codes to typed
    // cases; every other code, including the runtime's reserved negative
    // range (-1 generic, -2 panic, -3 marshalling), falls through to the
    // generic branded exception.
    assert!(
        kt.contains("1 -> NotFound(message)") && kt.contains("7 -> IoFailure(message)"),
        "declared codes must map to typed exception cases: {kt}"
    );
    assert!(
        kt.contains("else -> WeaveFFIException(code, message)"),
        "unknown and negative codes must fall through to the generic exception: {kt}"
    );
    assert!(
        !kt.contains("-1 ->") && !kt.contains("-2 ->") && !kt.contains("-3 ->"),
        "no reserved negative runtime code may be mapped to a typed case: {kt}"
    );
    let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
    // The throwing callable dispatches to the domain thrower (whose
    // `fromCode` still falls back generically for unknown codes); the
    // non-throwing callable traps straight through the generic thrower.
    let load = jni
        .find("Java_com_weaveffi_WeaveFFI_load")
        .expect("load export missing");
    let load_end = jni[load..].find("\n}\n").map_or(jni.len(), |e| load + e);
    assert!(
        jni[load..load_end].contains("throw_weaveffi_kv_KvError(env, &err);"),
        "throwing paths must dispatch to the typed domain thrower: {jni}"
    );
    let peek = jni
        .find("Java_com_weaveffi_WeaveFFI_peek")
        .expect("peek export missing");
    let peek_end = jni[peek..].find("\n}\n").map_or(jni.len(), |e| peek + e);
    assert!(
        jni[peek..peek_end].contains("throw_weaveffi_error(env, &err);"),
        "non-throwing paths must trap via the generic thrower: {jni}"
    );
}

#[test]
fn gradle_string_literals_survive_quotes_and_backslashes() {
    use crate::package::{build_gradle, gradle_squote, settings_gradle};
    assert_eq!(gradle_squote(r"it's a \test"), r"it\'s a \\test");
    let settings = settings_gradle("it's-a-lib", "weaveffi.yml");
    assert!(
        settings.contains(r"rootProject.name = 'it\'s-a-lib'"),
        "a quote in the project name must not terminate the Groovy literal: {settings}"
    );
    let gradle = build_gradle("com.weave'ffi", "weaveffi.yml");
    assert!(
        gradle.contains(r"namespace 'com.weave\'ffi'"),
        "a quote in the package must not terminate the namespace literal: {gradle}"
    );
}
