//! Once-per-file runtime support: the Kotlin value-buffer writer/reader, the
//! callback exception hook, the async continuation shim, and the JNI error
//! throwers, uncaught-exception plumbing, and listener registry.

use std::fmt::Write as _;

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{BindingModel, ErrorBinding, FnBinding};
use weaveffi_core::plan::ErrorStrategy;

use crate::entities::kotlin_exception_name;

/// Render the settable uncaught-exception hook into the `WeaveFFI` companion.
/// Listener callbacks and async continuation resume paths run on native
/// producer threads with no Kotlin caller up-stack, so a thrown exception has
/// nowhere to propagate; the JNI glue routes it here. When no handler is
/// installed, `dispatchCallbackException` rethrows, and the glue falls back to
/// `ExceptionDescribe` (logging the stack trace) before clearing.
pub(crate) fn render_kotlin_exception_handler_api(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("@Volatile private var callbackExceptionHandler: ((Throwable) -> Unit)? = null");
    w.blank();
    w.line("/**");
    w.line(" * Installs a handler for exceptions thrown by listener callbacks and");
    w.line(" * async continuations on native producer threads. These exceptions have");
    w.line(" * no Kotlin caller to propagate to; when no handler is installed, they");
    w.line(" * are logged with their stack trace and dropped. Pass `null` to");
    w.line(" * restore the default logging behavior.");
    w.line(" */");
    w.line("@JvmStatic fun setCallbackExceptionHandler(handler: ((Throwable) -> Unit)?) {");
    w.scope(|w| {
        w.line("callbackExceptionHandler = handler");
    });
    w.line("}");
    w.blank();
    w.line("// Invoked from the JNI glue; rethrowing signals \"no handler\" so the");
    w.line("// glue falls back to ExceptionDescribe.");
    w.line("@JvmStatic private fun dispatchCallbackException(t: Throwable) {");
    w.scope(|w| {
        w.line("callbackExceptionHandler?.invoke(t) ?: throw t");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the private Kotlin value-buffer runtime: a growable little-endian
/// writer, a validating reader (rejecting truncated buffers, invalid
/// bool/flag bytes, oversized length prefixes, and trailing bytes), and the
/// `weaveEncode`/`weaveDecode` entry points the generated wrappers call.
pub(crate) fn render_kotlin_buffer_runtime(out: &mut String) {
    let brand = errors::EXCEPTION_BRAND;
    let _ = write!(
        out,
        r#"
/** Growable little-endian writer implementing the WeaveFFI value-buffer wire format. */
internal class WeaveBufferWriter {{
    private var buf = ByteArray(32)
    private var len = 0

    private fun reserve(extra: Int) {{
        if (len + extra <= buf.size) return
        var cap = buf.size * 2
        while (cap < len + extra) cap *= 2
        buf = buf.copyOf(cap)
    }}

    fun toByteArray(): ByteArray = buf.copyOf(len)

    fun writeBool(v: Boolean) {{ reserve(1); buf[len++] = if (v) 1 else 0 }}
    fun writeI8(v: Byte) {{ reserve(1); buf[len++] = v }}
    fun writeI16(v: Short) {{
        reserve(2)
        val b = v.toInt()
        buf[len++] = (b and 0xFF).toByte()
        buf[len++] = ((b shr 8) and 0xFF).toByte()
    }}
    fun writeI32(v: Int) {{
        reserve(4)
        buf[len++] = (v and 0xFF).toByte()
        buf[len++] = ((v shr 8) and 0xFF).toByte()
        buf[len++] = ((v shr 16) and 0xFF).toByte()
        buf[len++] = ((v shr 24) and 0xFF).toByte()
    }}
    fun writeU32(v: Long) = writeI32(v.toInt())
    fun writeI64(v: Long) {{
        writeI32(v.toInt())
        writeI32((v ushr 32).toInt())
    }}
    fun writeF32(v: Float) = writeI32(v.toRawBits())
    fun writeF64(v: Double) = writeI64(v.toRawBits())
    fun writeString(v: String) = writeBytes(v.toByteArray(Charsets.UTF_8))
    fun writeBytes(v: ByteArray) {{
        writeI32(v.size)
        reserve(v.size)
        v.copyInto(buf, len)
        len += v.size
    }}
    fun <T> writeOptional(v: T?, write: (T) -> Unit) {{
        if (v == null) writeBool(false) else {{ writeBool(true); write(v) }}
    }}
    fun <T> writeList(v: List<T>, write: (T) -> Unit) {{
        writeI32(v.size)
        for (e in v) write(e)
    }}
    fun <K, V> writeMap(v: Map<K, V>, writeKey: (K) -> Unit, writeValue: (V) -> Unit) {{
        writeI32(v.size)
        for ((k, e) in v) {{ writeKey(k); writeValue(e) }}
    }}
}}

/** Validating little-endian reader for the WeaveFFI value-buffer wire format. */
internal class WeaveBufferReader(private val buf: ByteArray) {{
    private var pos = 0

    private fun malformed(detail: String): Nothing =
        throw {brand}(-2, "malformed WeaveFFI value buffer: " + detail)

    private fun take(n: Int): Int {{
        if (n > buf.size - pos) malformed("truncated buffer")
        val at = pos
        pos += n
        return at
    }}

    fun readBool(): Boolean = when (buf[take(1)].toInt()) {{
        0 -> false
        1 -> true
        else -> malformed("invalid bool byte")
    }}
    fun readI8(): Byte = buf[take(1)]
    fun readI16(): Short {{
        val at = take(2)
        return ((buf[at].toInt() and 0xFF) or ((buf[at + 1].toInt() and 0xFF) shl 8)).toShort()
    }}
    fun readI32(): Int {{
        val at = take(4)
        return (buf[at].toInt() and 0xFF) or
            ((buf[at + 1].toInt() and 0xFF) shl 8) or
            ((buf[at + 2].toInt() and 0xFF) shl 16) or
            ((buf[at + 3].toInt() and 0xFF) shl 24)
    }}
    fun readU32(): Long = readI32().toLong() and 0xFFFFFFFFL
    fun readI64(): Long {{
        val lo = readI32().toLong() and 0xFFFFFFFFL
        val hi = readI32().toLong()
        return lo or (hi shl 32)
    }}
    fun readF32(): Float = Float.fromBits(readI32())
    fun readF64(): Double = Double.fromBits(readI64())
    private fun readLen(): Int {{
        val n = readI32()
        if (n < 0 || n > buf.size - pos) malformed("length prefix exceeds remaining bytes")
        return n
    }}
    fun readString(): String {{
        val n = readLen()
        val at = take(n)
        return try {{
            Charsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(buf, at, n)).toString()
        }} catch (e: java.nio.charset.CharacterCodingException) {{
            malformed("string is not valid UTF-8")
        }}
    }}
    fun readBytes(): ByteArray {{
        val n = readLen()
        val at = take(n)
        return buf.copyOfRange(at, at + n)
    }}
    fun <T> readOptional(read: () -> T): T? = if (readBool()) read() else null
    fun <T> readList(read: () -> T): List<T> {{
        val n = readLen()
        val out = ArrayList<T>(n)
        repeat(n) {{ out.add(read()) }}
        return out
    }}
    fun <K, V> readMap(readKey: () -> K, readValue: () -> V): Map<K, V> {{
        val n = readLen()
        val out = LinkedHashMap<K, V>()
        repeat(n) {{ out[readKey()] = readValue() }}
        return out
    }}
    fun expectEnd() {{
        if (pos != buf.size) malformed("trailing bytes after value")
    }}
}}

internal fun weaveEncode(write: (WeaveBufferWriter) -> Unit): ByteArray {{
    val w = WeaveBufferWriter()
    write(w)
    return w.toByteArray()
}}

internal fun <T> weaveDecode(bytes: ByteArray, read: (WeaveBufferReader) -> T): T {{
    val r = WeaveBufferReader(bytes)
    val v = read(r)
    r.expectEnd()
    return v
}}
"#
    );
}

/// Render the `WeaveContinuation` shim boxing a cancellable continuation and
/// its error mapper for delivery through the JNI async callback.
pub(crate) fn render_weave_continuation(out: &mut String) {
    out.push_str("\ninternal class WeaveContinuation<T>(\n");
    out.push_str("    private val cont: kotlinx.coroutines.CancellableContinuation<T>,\n");
    out.push_str("    private val mapError: (Int, String, ByteArray?) -> Throwable\n");
    out.push_str(") {\n");
    out.push_str("    @Suppress(\"UNCHECKED_CAST\")\n");
    out.push_str("    fun onSuccess(result: Any?) { cont.resume(result as T) }\n");
    out.push_str("    fun onError(code: Int, message: String, payload: ByteArray?) { cont.resumeWithException(mapError(code, message, payload)) }\n");
    out.push_str("}\n");
}

/// Emit the uncaught-exception plumbing shared by listener trampolines,
/// callback invocations, and async continuation resumes: a `JNI_OnLoad` that
/// caches a global reference to the generated `WeaveFFI` class (producer
/// threads cannot `FindClass` app classes), and a helper that routes a
/// pending exception to the settable Kotlin handler. When no handler is
/// installed (the dispatcher rethrows) or the handler itself throws, the
/// helper falls back to `ExceptionDescribe`, so the exception is logged with
/// its stack trace before being cleared; it is never silently swallowed.
pub(crate) fn render_jni_uncaught_support(out: &mut String, jni_pkg_path: &str) {
    let mut w = CodeWriter::four_space();
    w.line("static jclass weaveffi_jni_entry_class = NULL;");
    w.line("static jmethodID weaveffi_jni_dispatch_exc = NULL;");
    w.blank();
    w.line("JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* reserved) {");
    w.scope(|w| {
        w.line("(void)reserved;");
        w.line("JNIEnv* env = NULL;");
        w.line("if ((*vm)->GetEnv(vm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) { return JNI_ERR; }");
        w.line(format!(
            "jclass cls = (*env)->FindClass(env, \"{jni_pkg_path}/WeaveFFI\");"
        ));
        w.line("if (cls == NULL) { (*env)->ExceptionClear(env); return JNI_VERSION_1_6; }");
        w.line("weaveffi_jni_entry_class = (jclass)(*env)->NewGlobalRef(env, cls);");
        w.line("weaveffi_jni_dispatch_exc = (*env)->GetStaticMethodID(env, weaveffi_jni_entry_class, \"dispatchCallbackException\", \"(Ljava/lang/Throwable;)V\");");
        w.line("if (weaveffi_jni_dispatch_exc == NULL) { (*env)->ExceptionClear(env); }");
        w.line("return JNI_VERSION_1_6;");
    });
    w.line("}");
    w.blank();
    w.line("static void weaveffi_jni_handle_uncaught(JNIEnv* env) {");
    w.scope(|w| {
        w.line("if (!(*env)->ExceptionCheck(env)) { return; }");
        w.line("jthrowable ex = (*env)->ExceptionOccurred(env);");
        w.line("(*env)->ExceptionClear(env);");
        w.block(
            "if (weaveffi_jni_entry_class != NULL && weaveffi_jni_dispatch_exc != NULL) {",
            "}",
            |w| {
                w.line("(*env)->CallStaticVoidMethod(env, weaveffi_jni_entry_class, weaveffi_jni_dispatch_exc, ex);");
                w.line("if (!(*env)->ExceptionCheck(env)) { (*env)->DeleteLocalRef(env, ex); return; }");
                w.line("(*env)->ExceptionClear(env);");
            },
        );
        w.line("(*env)->Throw(env, ex);");
        w.line("(*env)->ExceptionDescribe(env);");
        w.line("(*env)->ExceptionClear(env);");
        w.line("(*env)->DeleteLocalRef(env, ex);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the generic thrower: constructs the brand exception with the raw
/// `(code, message)` pair via `NewObject` (so unknown codes keep their numeric
/// code) and throws it. Every non-throwing callable dispatches here, and it
/// is the trap channel for the runtime's reserved negative codes.
pub(crate) fn render_jni_generic_thrower(out: &mut String, jni_pkg_path: &str) {
    let mut w = CodeWriter::four_space();
    w.line("static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {");
    w.scope(|w| {
        w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
        w.line(format!(
            "jclass exClass = (*env)->FindClass(env, \"{}/{}\");",
            jni_pkg_path,
            errors::EXCEPTION_BRAND
        ));
        w.block("if (exClass != NULL) {", "}", |w| {
            w.line("jmethodID ctor = (*env)->GetMethodID(env, exClass, \"<init>\", \"(ILjava/lang/String;)V\");");
            w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
            w.line("jthrowable ex = (jthrowable)(*env)->NewObject(env, exClass, ctor, (jint)err->code, jmsg);");
            w.line("if (ex != NULL) { (*env)->Throw(env, ex); }");
        });
        w.line("weaveffi_error_clear(err);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the thrower for one declared error domain: the raw `(code, message)`
/// pair and the serialized payload (copied to a `jbyteArray`, or `NULL` when
/// absent) are handed to the sealed exception's static `fromCode` factory,
/// which decodes payload fields into the typed subclass; unknown codes fall
/// back to the generic exception inside `fromCode` itself. Both the message
/// and the payload buffer are released via `weaveffi_error_clear`.
pub(crate) fn render_jni_domain_thrower(out: &mut String, eb: &ErrorBinding, jni_pkg_path: &str) {
    let exc = kotlin_exception_name(eb);
    let brand = errors::EXCEPTION_BRAND;
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void throw_{}(JNIEnv* env, weaveffi_error* err) {{",
        eb.c_tag
    ));
    w.scope(|w| {
        w.line(format!(
            "jclass exClass = (*env)->FindClass(env, \"{jni_pkg_path}/{exc}\");"
        ));
        w.line(format!(
            "jmethodID fromCode = exClass ? (*env)->GetStaticMethodID(env, exClass, \"fromCode\", \"(ILjava/lang/String;[B)L{jni_pkg_path}/{brand};\") : NULL;"
        ));
        w.block("if (fromCode == NULL) {", "}", |w| {
            w.line("(*env)->ExceptionClear(env);");
            w.line("throw_weaveffi_error(env, err);");
            w.line("return;");
        });
        w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
        w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
        w.line("jbyteArray jpayload = NULL;");
        w.block("if (err->payload_ptr != NULL) {", "}", |w| {
            w.line("jpayload = (*env)->NewByteArray(env, (jsize)err->payload_len);");
            w.line("if (jpayload != NULL) { (*env)->SetByteArrayRegion(env, jpayload, 0, (jsize)err->payload_len, (const jbyte*)err->payload_ptr); }");
        });
        w.line("jthrowable ex = (jthrowable)(*env)->CallStaticObjectMethod(env, exClass, fromCode, (jint)err->code, jmsg, jpayload);");
        // A pending exception from fromCode itself (e.g. a malformed payload
        // buffer) is left in place; otherwise the mapped exception is thrown.
        w.line("if (ex != NULL && !(*env)->ExceptionCheck(env)) { (*env)->Throw(env, ex); }");
        w.line("weaveffi_error_clear(err);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The C thrower a sync callable's error check dispatches to: the typed
/// domain thrower when the callable's [`ErrorStrategy`] is
/// [`Throws`](ErrorStrategy::Throws) inside a module with an error domain,
/// the generic thrower otherwise (the trap channel).
pub(crate) fn jni_thrower_for(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (error, f.error_strategy()) {
        (Some(eb), ErrorStrategy::Throws) => format!("throw_{}", eb.c_tag),
        _ => "throw_weaveffi_error".to_string(),
    }
}

/// Whether any sync or iterator callable dispatches to the domain thrower for
/// `c_tag`, counting inheriting submodules. Async errors bypass the C
/// throwers (they resume the continuation), so an async-only domain emits no
/// thrower.
pub(crate) fn domain_thrower_used(model: &BindingModel, c_tag: &str) -> bool {
    model.modules.iter().any(|m| {
        m.error.as_ref().is_some_and(|e| e.c_tag == c_tag)
            && m.callables().any(|f| f.throws && !f.is_async)
    })
}

/// The shared listener context + registry. Producers may fire events on any
/// thread, so registry mutation (register/unregister, both on JVM threads)
/// is mutex-guarded; trampolines only read their own context.
pub(crate) fn render_jni_listener_support(out: &mut String) {
    let mut w = CodeWriter::four_space();
    w.block(
        "typedef struct weaveffi_jni_listener_ctx {",
        "} weaveffi_jni_listener_ctx;",
        |w| {
            w.line("JavaVM* jvm;");
            w.line("jobject callback;");
            w.line("uint64_t id;");
            w.line("struct weaveffi_jni_listener_ctx* next;");
        },
    );
    w.blank();
    w.line("static weaveffi_jni_listener_ctx* weaveffi_jni_listeners = NULL;");
    w.line("static pthread_mutex_t weaveffi_jni_listener_lock = PTHREAD_MUTEX_INITIALIZER;");
    w.blank();
    out.push_str(&w.finish());
}
