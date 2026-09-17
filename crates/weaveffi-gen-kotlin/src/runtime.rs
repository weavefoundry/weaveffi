//! Once-per-file runtime support: the Kotlin native-library loader, the
//! native-reference holder and its `Cleaner`, the value-buffer writer/reader,
//! the async exception hook and continuation shim, and the JNI `JNI_OnLoad`,
//! thread-attach helpers, foreign-error reporter, and error throwers.

use std::fmt::Write as _;

use weaveffi_core::cabi::ABI_VERSION;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    BindingModel, CallbackInterfaceBinding, ErrorBinding, FnBinding, ModuleBinding,
};
use weaveffi_core::plan::ErrorStrategy;

use crate::entities::kotlin_exception_name;
use crate::types::{jni_descriptor, kotlin_callback_dispatch_name, kt_param};

/// The reserved runtime error code a callback trampoline reports when the
/// Kotlin implementation threw: "a consumer callback failed".
pub(crate) const FOREIGN_ERROR_CODE: i32 = -4;

/// How the generated Kotlin loads the JNI shim (and, when packaged, the
/// producer library it links against).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LibraryLoading {
    /// `System.loadLibrary(name)`: the shim is on the JVM's library path or
    /// bundled as an Android `jniLibs` entry.
    SystemLibrary(String),
    /// The packaged layout: on Android, `System.loadLibrary("{lib}_jni")`
    /// resolves the AAR's `jniLibs`; on a desktop JVM the producer library
    /// and the shim are extracted from `natives/<platform id>/` classpath
    /// resources and loaded with `System.load`, falling back to
    /// `System.loadLibrary` when the resources are absent.
    Packaged {
        /// The logical producer library base name (`libcontacts.so`).
        lib_name: String,
    },
}

/// The JNI shim library base name the packaged layout builds and loads:
/// `{lib}_jni`, so it never collides with the producer library itself.
pub(crate) fn jni_lib_name(lib_name: &str) -> String {
    format!("{lib_name}_jni")
}

/// Render the `WeaveNativeLibrary` object every class with `external`
/// members touches from its companion `init`, so the shim is loaded exactly
/// once before any native declaration is bound.
pub(crate) fn render_kotlin_native_library(out: &mut String, loading: &LibraryLoading) {
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line("/** Loads the native library once; every companion `init` calls [ensureLoaded]. */");
    w.line("internal object WeaveNativeLibrary {");
    w.scope(|w| match loading {
        LibraryLoading::SystemLibrary(name) => {
            w.line(format!(
                "init {{ System.loadLibrary(\"{}\") }}",
                name.replace('"', "\\\"")
            ));
            w.blank();
            w.line("fun ensureLoaded() {}");
        }
        LibraryLoading::Packaged { lib_name } => {
            let lib = lib_name.replace('"', "\\\"");
            let jni = jni_lib_name(&lib);
            w.line(format!("private const val LIB = \"{lib}\""));
            w.line(format!("private const val JNI_LIB = \"{jni}\""));
            w.blank();
            w.line("init {");
            w.scope(|w| {
                w.line("val vendor = System.getProperty(\"java.vm.vendor\") ?: \"\"");
                w.line("if (vendor.contains(\"Android\")) {");
                w.scope(|w| {
                    w.line("System.loadLibrary(JNI_LIB)");
                });
                w.line("} else {");
                w.scope(|w| {
                    w.line("loadFromResources()");
                });
                w.line("}");
            });
            w.line("}");
            w.blank();
            w.line("fun ensureLoaded() {}");
            w.blank();
            w.line("private fun loadFromResources() {");
            w.scope(|w| {
                w.line("val id = platformId()");
                w.line("val dir = java.nio.file.Files.createTempDirectory(\"weaveffi-natives\")");
                w.line("dir.toFile().deleteOnExit()");
                w.line("// The producer library first, so the shim's dependency resolves.");
                w.line("extract(id, fileName(LIB), dir)?.let { System.load(it) }");
                w.line("val shim = extract(id, fileName(JNI_LIB), dir)");
                w.line("if (shim != null) System.load(shim) else System.loadLibrary(JNI_LIB)");
            });
            w.line("}");
            w.blank();
            w.line("private fun platformId(): String {");
            w.scope(|w| {
                w.line("val os = System.getProperty(\"os.name\").lowercase()");
                w.line("val arch = System.getProperty(\"os.arch\").lowercase()");
                w.line("val osId = when {");
                w.scope(|w| {
                    w.line("os.contains(\"mac\") || os.contains(\"darwin\") -> \"darwin\"");
                    w.line("os.contains(\"win\") -> \"windows\"");
                    w.line("else -> \"linux\"");
                });
                w.line("}");
                w.line("val archId = if (arch == \"aarch64\" || arch == \"arm64\") \"arm64\" else \"x64\"");
                w.line("return \"$osId-$archId\"");
            });
            w.line("}");
            w.blank();
            w.line("private fun fileName(base: String): String {");
            w.scope(|w| {
                w.line("val os = System.getProperty(\"os.name\").lowercase()");
                w.line("return when {");
                w.scope(|w| {
                    w.line("os.contains(\"mac\") || os.contains(\"darwin\") -> \"lib$base.dylib\"");
                    w.line("os.contains(\"win\") -> \"$base.dll\"");
                    w.line("else -> \"lib$base.so\"");
                });
                w.line("}");
            });
            w.line("}");
            w.blank();
            w.line("private fun extract(id: String, name: String, dir: java.nio.file.Path): String? {");
            w.scope(|w| {
                w.line("val stream = WeaveNativeLibrary::class.java.getResourceAsStream(\"/natives/$id/$name\") ?: return null");
                w.line("val target = dir.resolve(name)");
                w.line("stream.use { java.nio.file.Files.copy(it, target) }");
                w.line("target.toFile().deleteOnExit()");
                w.line("return target.toAbsolutePath().toString()");
            });
            w.line("}");
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render the native-reference holder shared by every object wrapper and
/// iterator class: a `Runnable` owning one strong reference that is released
/// exactly once, either from the wrapper's `close()` or from the
/// `java.lang.ref.Cleaner` backstop when the wrapper becomes unreachable
/// without being closed. The holder never references the wrapper, which is
/// what lets the `Cleaner` collect it.
pub(crate) fn render_kotlin_object_runtime(out: &mut String) {
    let _ = write!(
        out,
        r#"
/**
 * One strong native reference, released exactly once. Wrappers register an
 * instance with [weaveCleaner] so an unreachable, never-closed wrapper still
 * releases its reference; an explicit `close()` runs the same action first
 * and the cleaner then does nothing.
 */
internal class WeaveNativeRef(handle: Long, private val release: (Long) -> Unit) : Runnable {{
    private val handle = java.util.concurrent.atomic.AtomicLong(handle)

    /** The live pointer, borrowed for the duration of one native call. */
    fun get(): Long {{
        val h = handle.get()
        check(h != 0L) {{ "WeaveFFI object used after close()" }}
        return h
    }}

    /** The live pointer, or `0L` once released. */
    fun peek(): Long = handle.get()

    override fun run() {{
        val h = handle.getAndSet(0L)
        if (h != 0L) release(h)
    }}
}}

/** The process-wide cleaner backing every generated wrapper's disposal backstop. */
internal val weaveCleaner: java.lang.ref.Cleaner = java.lang.ref.Cleaner.create()
"#
    );
}

/// Render the settable uncaught-exception hook into the `WeaveFFI` companion.
/// Async continuation resume paths run on native producer threads with no
/// Kotlin caller up-stack, so a thrown exception has nowhere to propagate;
/// the JNI glue routes it here. When no handler is installed,
/// `dispatchCallbackException` rethrows, and the glue falls back to
/// `ExceptionDescribe` (logging the stack trace) before clearing.
pub(crate) fn render_kotlin_exception_handler_api(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("@Volatile private var callbackExceptionHandler: ((Throwable) -> Unit)? = null");
    w.blank();
    w.line("/**");
    w.line(" * Installs a handler for exceptions thrown while an async result is");
    w.line(" * delivered on a native producer thread. These exceptions have no Kotlin");
    w.line(" * caller to propagate to; when no handler is installed, they are logged");
    w.line(" * with their stack trace and dropped. Pass `null` to restore the default");
    w.line(" * logging behavior. Exceptions thrown by callback interface methods are");
    w.line(" * not routed here: they abort the producer call that invoked them and");
    w.line(" * surface to the original caller as a [WeaveFFIException] with code -4.");
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
/// bool/flag bytes, zero object tokens, oversized length prefixes, and
/// trailing bytes), and the `weaveEncode`/`weaveDecode` entry points the
/// generated wrappers call.
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
    /** An object token: one strong reference the caller adopts. Zero is never valid here. */
    fun readObject(): Long {{
        val token = readI64()
        if (token == 0L) malformed("null object token")
        return token
    }}
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

/// The C identifier of the cached `jclass` global reference for a callback
/// interface's Kotlin dispatch object.
pub(crate) fn jni_cb_class_var(cb: &CallbackInterfaceBinding) -> String {
    format!("{}_jni_cls", cb.c_tag)
}

/// The C identifier of the cached `jmethodID` for one dispatch shim of a
/// callback interface.
pub(crate) fn jni_cb_method_var(cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("{}_jni_mid_{method}", cb.c_tag)
}

/// Emit the file-scope JNI state and `JNI_OnLoad`: the cached `JavaVM*` every
/// producer-thread entry attaches through, the ABI revision check, and, as
/// the model needs them, the cached `WeaveFFI` entry class for async
/// exception routing and one cached class plus per-method `jmethodID` set for
/// each callback interface's Kotlin dispatch object. Everything is resolved
/// here because `System.loadLibrary` runs `JNI_OnLoad` on an app thread whose
/// class loader can see the generated classes; producer threads cannot
/// `FindClass` them later.
pub(crate) fn render_jni_onload(
    out: &mut String,
    jni_pkg_path: &str,
    c_prefix: &str,
    has_async: bool,
    callback_interfaces: &[(&ModuleBinding, &CallbackInterfaceBinding)],
) {
    let mut w = CodeWriter::four_space();
    w.line("static JavaVM* weaveffi_jni_vm = NULL;");
    if has_async {
        w.line("static jclass weaveffi_jni_entry_class = NULL;");
        w.line("static jmethodID weaveffi_jni_dispatch_exc = NULL;");
    }
    if !callback_interfaces.is_empty() {
        w.line("static jmethodID weaveffi_jni_throwable_to_string = NULL;");
        for (_, cb) in callback_interfaces {
            w.line(format!("static jclass {} = NULL;", jni_cb_class_var(cb)));
            for m in &cb.methods {
                w.line(format!(
                    "static jmethodID {} = NULL;",
                    jni_cb_method_var(cb, &m.name)
                ));
            }
        }
    }
    w.blank();
    w.line("static jint weaveffi_jni_load_error(JNIEnv* env, const char* message) {");
    w.scope(|w| {
        w.line("jclass cls = (*env)->FindClass(env, \"java/lang/UnsatisfiedLinkError\");");
        w.line("if (cls != NULL) { (*env)->ThrowNew(env, cls, message); }");
        w.line("return JNI_ERR;");
    });
    w.line("}");
    w.blank();
    w.line("JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* reserved) {");
    w.scope(|w| {
        w.line("(void)reserved;");
        w.line("weaveffi_jni_vm = vm;");
        w.line("JNIEnv* env = NULL;");
        w.line("if ((*vm)->GetEnv(vm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) { return JNI_ERR; }");
        w.block(
            format!("if ({c_prefix}_abi_version() != {ABI_VERSION}u) {{"),
            "}",
            |w| {
                w.line(format!(
                    "return weaveffi_jni_load_error(env, \"WeaveFFI: the native library implements a different C ABI revision than these bindings (expected {ABI_VERSION})\");"
                ));
            },
        );
        if has_async {
            w.line(format!(
                "jclass entry = (*env)->FindClass(env, \"{jni_pkg_path}/WeaveFFI\");"
            ));
            w.line("if (entry == NULL) { return JNI_ERR; }");
            w.line("weaveffi_jni_entry_class = (jclass)(*env)->NewGlobalRef(env, entry);");
            w.line("weaveffi_jni_dispatch_exc = (*env)->GetStaticMethodID(env, weaveffi_jni_entry_class, \"dispatchCallbackException\", \"(Ljava/lang/Throwable;)V\");");
            w.line("if (weaveffi_jni_dispatch_exc == NULL) { (*env)->ExceptionClear(env); }");
        }
        if !callback_interfaces.is_empty() {
            w.line("jclass throwable = (*env)->FindClass(env, \"java/lang/Throwable\");");
            w.line("if (throwable == NULL) { return JNI_ERR; }");
            w.line("weaveffi_jni_throwable_to_string = (*env)->GetMethodID(env, throwable, \"toString\", \"()Ljava/lang/String;\");");
            w.line("if (weaveffi_jni_throwable_to_string == NULL) { return JNI_ERR; }");
            for (_, cb) in callback_interfaces {
                let cls = jni_cb_class_var(cb);
                let dispatch = kotlin_callback_dispatch_name(&cb.name);
                w.line(format!(
                    "jclass {cls}_local = (*env)->FindClass(env, \"{jni_pkg_path}/{dispatch}\");"
                ));
                w.line(format!("if ({cls}_local == NULL) {{ return JNI_ERR; }}"));
                w.line(format!(
                    "{cls} = (jclass)(*env)->NewGlobalRef(env, {cls}_local);"
                ));
                for m in &cb.methods {
                    let sig = jni_cb_shim_descriptor(cb, m, jni_pkg_path);
                    let mid = jni_cb_method_var(cb, &m.name);
                    w.line(format!(
                        "{mid} = (*env)->GetStaticMethodID(env, {cls}, \"{}\", \"{sig}\");",
                        kt_param(&m.name)
                    ));
                    w.line(format!("if ({mid} == NULL) {{ return JNI_ERR; }}"));
                }
            }
        }
        w.line("return JNI_VERSION_1_6;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The JNI method descriptor of a callback interface's dispatch shim: the
/// implementing object first, then every parameter as the shim receives it,
/// then the direct-family (or `V`) return.
pub(crate) fn jni_cb_shim_descriptor(
    cb: &CallbackInterfaceBinding,
    m: &weaveffi_core::model::CallbackMethodBinding,
    jni_pkg_path: &str,
) -> String {
    let mut sig = format!("(L{jni_pkg_path}/{};", cb.name);
    for p in &m.params {
        sig.push_str(jni_descriptor(Some(&p.ty)));
    }
    sig.push(')');
    sig.push_str(jni_descriptor(m.ret.as_ref()));
    sig
}

/// Emit the two string converters every `string` crossing goes through.
/// JNI's own `GetStringUTFChars`/`NewStringUTF` speak *modified* UTF-8, which
/// encodes supplementary characters (emoji, for instance) as CESU-8 surrogate
/// pairs: a producer expecting standard UTF-8 rejects such a parameter as
/// invalid, and a standard 4-byte sequence handed to `NewStringUTF` is
/// mangled. The converters go through UTF-16 (`GetStringChars`/`NewString`)
/// instead, so every Kotlin `String` round-trips byte-exact.
pub(crate) fn render_jni_string_helpers(out: &mut String) {
    let mut w = CodeWriter::four_space();
    w.line("/* Copies `s` as a NUL-terminated standard UTF-8 string (JNI's modified");
    w.line("   UTF-8 would encode supplementary characters as surrogate pairs the");
    w.line("   producer rejects). Lone surrogates become U+FFFD. Release with free().");
    w.line("   Returns NULL for a NULL string, for a string holding U+0000 (which no");
    w.line("   C string can carry), or on allocation failure; the producer then");
    w.line("   reports the null argument through its marshalling error. */");
    w.line("static char* weaveffi_jni_string_to_utf8(JNIEnv* env, jstring s) {");
    w.scope(|w| {
        w.line("if (s == NULL) { return NULL; }");
        w.line("jsize n = (*env)->GetStringLength(env, s);");
        w.line("const jchar* u = (*env)->GetStringChars(env, s, NULL);");
        w.line("if (u == NULL) { return NULL; }");
        w.line("char* out = (char*)malloc((size_t)n * 4u + 1u);");
        w.line("if (out == NULL) { (*env)->ReleaseStringChars(env, s, u); return NULL; }");
        w.line("size_t o = 0;");
        w.block("for (jsize i = 0; i < n; i++) {", "}", |w| {
            w.line("uint32_t c = u[i];");
            w.line("if (c == 0u) { free(out); (*env)->ReleaseStringChars(env, s, u); return NULL; }");
            w.block(
                "if (c >= 0xD800u && c <= 0xDBFFu && i + 1 < n && u[i + 1] >= 0xDC00u && u[i + 1] <= 0xDFFFu) {",
                "}",
                |w| {
                    w.line("c = 0x10000u + ((c - 0xD800u) << 10) + ((uint32_t)u[i + 1] - 0xDC00u);");
                    w.line("i++;");
                },
            );
            w.line("else if (c >= 0xD800u && c <= 0xDFFFu) { c = 0xFFFDu; }");
            w.line("if (c < 0x80u) { out[o++] = (char)c; }");
            w.line("else if (c < 0x800u) { out[o++] = (char)(0xC0u | (c >> 6)); out[o++] = (char)(0x80u | (c & 0x3Fu)); }");
            w.line("else if (c < 0x10000u) { out[o++] = (char)(0xE0u | (c >> 12)); out[o++] = (char)(0x80u | ((c >> 6) & 0x3Fu)); out[o++] = (char)(0x80u | (c & 0x3Fu)); }");
            w.line("else { out[o++] = (char)(0xF0u | (c >> 18)); out[o++] = (char)(0x80u | ((c >> 12) & 0x3Fu)); out[o++] = (char)(0x80u | ((c >> 6) & 0x3Fu)); out[o++] = (char)(0x80u | (c & 0x3Fu)); }");
        });
        w.line("out[o] = 0;");
        w.line("(*env)->ReleaseStringChars(env, s, u);");
        w.line("return out;");
    });
    w.line("}");
    w.blank();
    w.line("/* Builds a jstring from standard UTF-8 (NewStringUTF expects modified UTF-8");
    w.line("   and mangles supplementary characters). NULL yields the empty string and");
    w.line("   a malformed sequence yields U+FFFD, so this never fails on producer");
    w.line("   output; it returns NULL only when the JVM could not allocate. */");
    w.line("static jstring weaveffi_jni_utf8_to_string(JNIEnv* env, const char* s) {");
    w.scope(|w| {
        w.line("if (s == NULL) { s = \"\"; }");
        w.line("size_t n = strlen(s);");
        w.line("jchar* buf = (jchar*)malloc((n + 1u) * sizeof(jchar));");
        w.line("if (buf == NULL) { return NULL; }");
        w.line("size_t i = 0, o = 0;");
        w.block("while (i < n) {", "}", |w| {
            w.line("unsigned char b = (unsigned char)s[i];");
            w.line("uint32_t c; size_t len;");
            w.line("if (b < 0x80u) { c = b; len = 1; }");
            w.line("else if ((b & 0xE0u) == 0xC0u) { c = b & 0x1Fu; len = 2; }");
            w.line("else if ((b & 0xF0u) == 0xE0u) { c = b & 0x0Fu; len = 3; }");
            w.line("else if ((b & 0xF8u) == 0xF0u) { c = b & 0x07u; len = 4; }");
            w.line("else { c = 0xFFFDu; len = 1; }");
            w.line("if (i + len > n) { c = 0xFFFDu; len = 1; }");
            w.block("for (size_t k = 1; k < len; k++) {", "}", |w| {
                w.line("unsigned char cb = (unsigned char)s[i + k];");
                w.line("if ((cb & 0xC0u) != 0x80u) { c = 0xFFFDu; len = k; break; }");
                w.line("c = (c << 6) | (cb & 0x3Fu);");
            });
            w.line("i += len;");
            w.line("if (c > 0x10FFFFu) { c = 0xFFFDu; }");
            w.line("if (c >= 0x10000u) { c -= 0x10000u; buf[o++] = (jchar)(0xD800u + (c >> 10)); buf[o++] = (jchar)(0xDC00u + (c & 0x3FFu)); }");
            w.line("else { buf[o++] = (jchar)c; }");
        });
        w.line("jstring out = (*env)->NewString(env, buf, (jsize)o);");
        w.line("free(buf);");
        w.line("return out;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the thread-attach helpers every producer-thread entry point (async
/// completion callbacks, callback-interface trampolines, and vtable `free`)
/// uses. A producer thread is usually not a JVM thread: attach when needed
/// and detach before returning, because a thread that dies while still
/// attached leaves the JVM with a zombie attachment record that hangs
/// process shutdown.
pub(crate) fn render_jni_thread_helpers(out: &mut String) {
    let mut w = CodeWriter::four_space();
    w.line("/* Returns 1 when this call attached the thread (the caller must detach),");
    w.line("   0 otherwise. *env is NULL when no JNIEnv could be obtained. */");
    w.line("static int weaveffi_jni_attach(JNIEnv** env) {");
    w.scope(|w| {
        w.line("*env = NULL;");
        w.line("if (weaveffi_jni_vm == NULL) { return 0; }");
        w.line("if ((*weaveffi_jni_vm)->GetEnv(weaveffi_jni_vm, (void**)env, JNI_VERSION_1_6) == JNI_OK) { return 0; }");
        // `AttachCurrentThread` takes `JNIEnv**` on Android and `void**`
        // elsewhere; a `void*` converts implicitly to either in C.
        w.line("if ((*weaveffi_jni_vm)->AttachCurrentThread(weaveffi_jni_vm, (void*)env, NULL) != JNI_OK) { *env = NULL; return 0; }");
        w.line("return 1;");
    });
    w.line("}");
    w.blank();
    w.line("static void weaveffi_jni_detach(int attached) {");
    w.scope(|w| {
        w.line("if (attached) { (*weaveffi_jni_vm)->DetachCurrentThread(weaveffi_jni_vm); }");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the uncaught-exception helper for async continuation resumes: a
/// pending exception is routed to the settable Kotlin handler through the
/// cached `WeaveFFI` class. When no handler is installed (the dispatcher
/// rethrows) or the handler itself throws, the helper falls back to
/// `ExceptionDescribe`, so the exception is logged with its stack trace
/// before being cleared; it is never silently swallowed.
pub(crate) fn render_jni_uncaught_support(out: &mut String) {
    let mut w = CodeWriter::four_space();
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

/// Emit the foreign-failure reporter callback trampolines call when the
/// Kotlin implementation (or the decoding shim in front of it) threw: the
/// pending exception is cleared, its `toString()` is borrowed, and
/// `{prefix}_error_set(out_err, -4, message)` hands the producer a copy. The
/// exception never unwinds through the C frame.
pub(crate) fn render_jni_foreign_error_support(out: &mut String, c_prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.line("static void weaveffi_jni_report_foreign(JNIEnv* env, weaveffi_error* out_err) {");
    w.scope(|w| {
        w.line("jthrowable ex = (*env)->ExceptionOccurred(env);");
        w.line("(*env)->ExceptionClear(env);");
        w.line("jstring jmsg = NULL;");
        w.block(
            "if (ex != NULL && weaveffi_jni_throwable_to_string != NULL) {",
            "}",
            |w| {
                w.line("jmsg = (jstring)(*env)->CallObjectMethod(env, ex, weaveffi_jni_throwable_to_string);");
                w.line("if ((*env)->ExceptionCheck(env)) { (*env)->ExceptionClear(env); jmsg = NULL; }");
            },
        );
        w.line("char* msg = weaveffi_jni_string_to_utf8(env, jmsg);");
        w.line(format!(
            "{c_prefix}_error_set(out_err, {FOREIGN_ERROR_CODE}, msg != NULL ? msg : \"callback interface implementation threw\");"
        ));
        w.line("free(msg);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the generic thrower: constructs the brand exception with the raw
/// `(code, message)` pair via `NewObject` (so unknown codes keep their numeric
/// code) and throws it. Every non-throwing callable dispatches here, and it
/// is the trap channel for the runtime's reserved negative codes (`-1`
/// generic, `-2` producer panic, `-3` marshalling failure, `-4` a callback
/// interface implementation raised).
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
            w.line("jstring jmsg = weaveffi_jni_utf8_to_string(env, msg);");
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
        w.line("jstring jmsg = weaveffi_jni_utf8_to_string(env, msg);");
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
