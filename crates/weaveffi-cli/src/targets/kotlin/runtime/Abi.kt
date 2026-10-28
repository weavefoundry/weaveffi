package {{PACKAGE}}

import com.sun.jna.Callback
import com.sun.jna.IntegerType
import com.sun.jna.Library
import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.Structure

/** A C `size_t`. */
internal class SizeT @JvmOverloads constructor(value: Long = 0) :
    IntegerType(Native.SIZE_T_SIZE, value, true) {
    override fun toByte(): Byte = toLong().toByte()

    override fun toShort(): Short = toLong().toShort()
}

/**
 * A C `{{PREFIX}}_run`: a string's UTF-8, raw bytes, a typed array, or a value
 * buffer, `len` bytes at `ptr`. Null with length 0 is the empty run. As a
 * structure it's the by-reference form (an iterator's out item); [ByValue]
 * is what crosses everywhere else.
 */
@Structure.FieldOrder("ptr", "len")
internal open class Run : Structure {
    @JvmField
    var ptr: Pointer? = null

    @JvmField
    var len: SizeT = SizeT()

    constructor() : super()

    constructor(ptr: Pointer?, len: Long) : super() {
        this.ptr = ptr
        this.len = SizeT(len)
        write()
    }

    /** The run by value. */
    class ByValue : Run, Structure.ByValue {
        constructor() : super()

        constructor(ptr: Pointer?, len: Long) : super(ptr, len)
    }
}

/**
 * A C `{{PREFIX}}_error`: the slot every synchronous call overwrites, and the
 * heap-boxed error an async completion receives.
 */
@Structure.FieldOrder("code", "message_ptr", "message_len", "payload_ptr", "payload_len")
internal class NativeError : Structure {
    @JvmField
    var code: Int = 0

    @JvmField
    var message_ptr: Pointer? = null

    @JvmField
    var message_len: SizeT = SizeT()

    @JvmField
    var payload_ptr: Pointer? = null

    @JvmField
    var payload_len: SizeT = SizeT()

    constructor() : super()

    /** The error the library boxed at [pointer]. */
    constructor(pointer: Pointer) : super(pointer) {
        read()
    }

    /** The message (UTF-8, not NUL-terminated). */
    fun message(): String = decodeUtf8(bytesAt(message_ptr, message_len))

    /** The code's fields as a value buffer, or `null` when it has none. */
    fun payload(): ByteArray? = payload_ptr?.let { bytesAt(it, payload_len) }
}

/** A C `{{PREFIX}}_{Cb}` argument: an implementation's context and its vtable. */
@Structure.FieldOrder("ctx", "vtable")
internal class CallbackArg : Structure, Structure.ByValue {
    @JvmField
    var ctx: Pointer? = null

    @JvmField
    var vtable: Pointer? = null

    /** No implementation (a null vtable). */
    constructor() : super()

    constructor(ctx: Pointer?, vtable: Pointer?) : super() {
        this.ctx = ctx
        this.vtable = vtable
        write()
    }
}

/** A vtable's `free` entry. */
internal fun interface FreeCallback : Callback {
    fun invoke(ctx: Pointer?)
}

/** The runtime surface every producer exports. */
internal interface CRuntime : Library {
    fun {{PREFIX}}_abi_version(): Int

    fun {{PREFIX}}_error_set(err: Pointer?, code: Int, message: ByteArray?, len: SizeT)

    fun {{PREFIX}}_error_set_payload(err: Pointer?, payload: ByteArray?, len: SizeT)

    fun {{PREFIX}}_error_clear(err: NativeError?)

    fun {{PREFIX}}_error_free(err: Pointer?)

    fun {{PREFIX}}_alloc(len: SizeT): Pointer?

    fun {{PREFIX}}_run_free(run: Run.ByValue)

    fun {{PREFIX}}_cancel_token_create(): Pointer?

    fun {{PREFIX}}_cancel_token_cancel(token: Pointer?)

    fun {{PREFIX}}_cancel_token_is_cancelled(token: Pointer?): Byte

    fun {{PREFIX}}_cancel_token_destroy(token: Pointer?)

    fun {{PREFIX}}_debug_live(kind: Int): Long
}

/** A C `bool`, which crosses as one byte. */
internal fun Boolean.asByte(): Byte = if (this) 1 else 0

/** A C `bool` that crossed as one byte. */
internal fun Byte.asBoolean(): Boolean = this != 0.toByte()

private fun bytesAt(pointer: Pointer?, len: SizeT): ByteArray {
    val n = len.toLong()
    if (pointer == null || n == 0L) return ByteArray(0)
    if (n > Int.MAX_VALUE) throw NativeBugException(-3, "a run of $n bytes is too long")
    return pointer.getByteArray(0, n.toInt())
}

/** The element count of a typed-array run of [size]-byte elements. */
private fun Run.count(size: Int): Int {
    val n = len.toLong()
    if (n % size != 0L) throw NativeBugException(-3, "a typed array of $n bytes isn't a whole number of elements")
    if (n / size > Int.MAX_VALUE) throw NativeBugException(-3, "a typed array of $n bytes is too long")
    return (n / size).toInt()
}

/** This run's bytes, copied. */
internal fun Run.bytes(): ByteArray = bytesAt(ptr, len)

/** This run's UTF-8, decoded. */
internal fun Run.string(): String = decodeUtf8(bytes())

/** This run's elements, as a `[i16]` or `[u16]`. */
internal fun Run.shorts(): ShortArray = count(2).let { n -> ptr?.getShortArray(0, n) ?: ShortArray(0) }

/** This run's elements, as an `[i32]` or `[u32]`. */
internal fun Run.ints(): IntArray = count(4).let { n -> ptr?.getIntArray(0, n) ?: IntArray(0) }

/** This run's elements, as an `[i64]` or `[u64]`. */
internal fun Run.longs(): LongArray = count(8).let { n -> ptr?.getLongArray(0, n) ?: LongArray(0) }

/** This run's elements, as an `[f32]`. */
internal fun Run.floats(): FloatArray = count(4).let { n -> ptr?.getFloatArray(0, n) ?: FloatArray(0) }

/** This run's elements, as an `[f64]`. */
internal fun Run.doubles(): DoubleArray = count(8).let { n -> ptr?.getDoubleArray(0, n) ?: DoubleArray(0) }

/** Reads a run the library returned with [read], then releases it. */
internal inline fun <T> take(run: Run, read: (Run) -> T): T {
    try {
        return read(run)
    } finally {
        freeRun(run)
    }
}

/** Releases a run the library returned. */
internal fun freeRun(run: Run) {
    if (run.ptr == null) return
    Lib.c.{{PREFIX}}_run_free(run as? Run.ByValue ?: Run.ByValue(run.ptr, run.len.toLong()))
}

/** A run of [len] bytes the library borrows for one call, filled by [fill]. */
private inline fun staged(len: Long, fill: (Pointer) -> Unit): Run.ByValue {
    if (len == 0L) return Run.ByValue()
    val memory = Memory(len)
    fill(memory)
    return Run.ByValue(memory, len)
}

/** [bytes] as a run the library borrows. */
internal fun runOf(bytes: ByteArray): Run.ByValue = staged(bytes.size.toLong()) { it.write(0, bytes, 0, bytes.size) }

/** [values] as a run the library borrows. */
internal fun runOf(values: ShortArray): Run.ByValue = staged(values.size * 2L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library borrows. */
internal fun runOf(values: IntArray): Run.ByValue = staged(values.size * 4L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library borrows. */
internal fun runOf(values: LongArray): Run.ByValue = staged(values.size * 8L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library borrows. */
internal fun runOf(values: FloatArray): Run.ByValue = staged(values.size * 4L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library borrows. */
internal fun runOf(values: DoubleArray): Run.ByValue = staged(values.size * 8L) { it.write(0, values, 0, values.size) }

/** A run of [len] bytes from `{{PREFIX}}_alloc`, which the library adopts, filled by [fill]. */
private inline fun allocated(len: Long, fill: (Pointer) -> Unit): Run.ByValue {
    if (len == 0L) return Run.ByValue()
    val pointer = Lib.c.{{PREFIX}}_alloc(SizeT(len)) ?: throw OutOfMemoryError("{{PREFIX}}_alloc($len) failed")
    fill(pointer)
    return Run.ByValue(pointer, len)
}

/** [bytes] as a run the library adopts (a callback's return). */
internal fun allocRun(bytes: ByteArray): Run.ByValue = allocated(bytes.size.toLong()) { it.write(0, bytes, 0, bytes.size) }

/** [values] as a run the library adopts (a callback's return). */
internal fun allocRun(values: ShortArray): Run.ByValue = allocated(values.size * 2L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library adopts (a callback's return). */
internal fun allocRun(values: IntArray): Run.ByValue = allocated(values.size * 4L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library adopts (a callback's return). */
internal fun allocRun(values: LongArray): Run.ByValue = allocated(values.size * 8L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library adopts (a callback's return). */
internal fun allocRun(values: FloatArray): Run.ByValue = allocated(values.size * 4L) { it.write(0, values, 0, values.size) }

/** [values] as a run the library adopts (a callback's return). */
internal fun allocRun(values: DoubleArray): Run.ByValue = allocated(values.size * 8L) { it.write(0, values, 0, values.size) }
{{OPTIONALS}}
