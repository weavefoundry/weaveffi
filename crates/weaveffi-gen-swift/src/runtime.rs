//! The private Swift runtime prelude: the value-buffer writer/reader pair,
//! the generic brand error with its `check`/`trap` helpers, the boxed
//! continuation async wrappers thread through the C context slot, and the
//! foreign-error helper callback-interface trampolines report through.

use weaveffi_core::errors::ERROR_BRAND;

/// The `weaveffi_error.code` a callback-interface trampoline reports when the
/// consumer's implementation throws. Mirrors `weaveffi_abi::FOREIGN_ERROR_CODE`.
const FOREIGN_ERROR_CODE: i32 = -4;

/// The private Swift buffer runtime implementing the WeaveFFI value-buffer
/// wire format: little-endian, packed, no alignment. `WvWriter` serializes,
/// `WvReader` deserializes and traps (via `wvDecodeFailure`) on malformed
/// input, which the spec routes through the same channel as a producer panic.
///
/// Object tokens (`writeObject`/`readObject`) are the `u64` widening of an
/// interface pointer; each carries one strong reference, so the writer is
/// handed a freshly cloned pointer and the reader hands its pointer to a
/// wrapper that adopts it.
const BUFFER_RUNTIME: &str = r#"/// Serializes values into the WeaveFFI value-buffer wire format
/// (little-endian, packed, no alignment).
struct WvWriter {
    var bytes: [UInt8] = []

    mutating func writeBool(_ v: Bool) { bytes.append(v ? 1 : 0) }
    mutating func writeInt8(_ v: Int8) { bytes.append(UInt8(bitPattern: v)) }
    mutating func writeUInt8(_ v: UInt8) { bytes.append(v) }
    mutating func writeUInt16(_ v: UInt16) {
        bytes.append(UInt8(truncatingIfNeeded: v))
        bytes.append(UInt8(truncatingIfNeeded: v >> 8))
    }
    mutating func writeInt16(_ v: Int16) { writeUInt16(UInt16(bitPattern: v)) }
    mutating func writeUInt32(_ v: UInt32) {
        bytes.append(UInt8(truncatingIfNeeded: v))
        bytes.append(UInt8(truncatingIfNeeded: v >> 8))
        bytes.append(UInt8(truncatingIfNeeded: v >> 16))
        bytes.append(UInt8(truncatingIfNeeded: v >> 24))
    }
    mutating func writeInt32(_ v: Int32) { writeUInt32(UInt32(bitPattern: v)) }
    mutating func writeUInt64(_ v: UInt64) {
        writeUInt32(UInt32(truncatingIfNeeded: v))
        writeUInt32(UInt32(truncatingIfNeeded: v >> 32))
    }
    mutating func writeInt64(_ v: Int64) { writeUInt64(UInt64(bitPattern: v)) }
    mutating func writeFloat(_ v: Float) { writeUInt32(v.bitPattern) }
    mutating func writeDouble(_ v: Double) { writeUInt64(v.bitPattern) }
    mutating func writeLen(_ n: Int) {
        precondition(n >= 0 && n <= Int(UInt32.max), "WeaveFFI buffer length exceeds UInt32.max")
        writeUInt32(UInt32(n))
    }
    mutating func writeString(_ v: String) {
        let utf8 = Array(v.utf8)
        writeLen(utf8.count)
        bytes.append(contentsOf: utf8)
    }
    mutating func writeBytes(_ v: Data) {
        writeLen(v.count)
        bytes.append(contentsOf: v)
    }
    mutating func writeOptionFlag(_ present: Bool) { bytes.append(present ? 1 : 0) }
    /// Writes an object token. `p` must be a strong reference the buffer now
    /// owns (a freshly cloned pointer), never one a wrapper still holds.
    mutating func writeObject(_ p: OpaquePointer) { writeUInt64(UInt64(UInt(bitPattern: p))) }
}

/// Traps on a malformed value buffer. Per the wire-format spec, consumers
/// surface decode failures through the same channel as a producer panic.
func wvDecodeFailure(_ context: String) -> Never {
    fatalError("malformed WeaveFFI value buffer: \(context)")
}

/// Deserializes values from the WeaveFFI value-buffer wire format, rejecting
/// truncated buffers, invalid flag bytes, oversized length prefixes, and
/// trailing bytes.
struct WvReader {
    let bytes: [UInt8]
    var pos: Int = 0

    init(bytes: [UInt8]) { self.bytes = bytes }

    var remaining: Int { bytes.count - pos }

    mutating func take(_ n: Int, _ context: String) -> ArraySlice<UInt8> {
        guard remaining >= n else { wvDecodeFailure(context) }
        defer { pos += n }
        return bytes[pos..<(pos + n)]
    }

    mutating func readBool() -> Bool {
        switch take(1, "bool").first! {
        case 0: return false
        case 1: return true
        default: wvDecodeFailure("bool byte out of range")
        }
    }
    mutating func readUInt8() -> UInt8 { take(1, "u8").first! }
    mutating func readInt8() -> Int8 { Int8(bitPattern: readUInt8()) }
    mutating func readUInt16() -> UInt16 {
        var v: UInt16 = 0
        for (i, b) in take(2, "u16").enumerated() { v |= UInt16(b) << (8 * i) }
        return v
    }
    mutating func readInt16() -> Int16 { Int16(bitPattern: readUInt16()) }
    mutating func readUInt32() -> UInt32 {
        var v: UInt32 = 0
        for (i, b) in take(4, "u32").enumerated() { v |= UInt32(b) << (8 * i) }
        return v
    }
    mutating func readInt32() -> Int32 { Int32(bitPattern: readUInt32()) }
    mutating func readUInt64() -> UInt64 {
        var v: UInt64 = 0
        for (i, b) in take(8, "u64").enumerated() { v |= UInt64(b) << (8 * i) }
        return v
    }
    mutating func readInt64() -> Int64 { Int64(bitPattern: readUInt64()) }
    mutating func readFloat() -> Float { Float(bitPattern: readUInt32()) }
    mutating func readDouble() -> Double { Double(bitPattern: readUInt64()) }
    mutating func readLen() -> Int {
        let n = Int(readUInt32())
        guard n <= remaining else { wvDecodeFailure("length prefix exceeds remaining buffer") }
        return n
    }
    mutating func readString() -> String {
        let n = readLen()
        guard let s = String(bytes: take(n, "string bytes"), encoding: .utf8) else {
            wvDecodeFailure("string is not valid UTF-8")
        }
        return s
    }
    mutating func readBytes() -> Data {
        let n = readLen()
        return Data(take(n, "byte buffer"))
    }
    mutating func readOptionFlag() -> Bool {
        switch take(1, "option flag").first! {
        case 0: return false
        case 1: return true
        default: wvDecodeFailure("option flag byte out of range")
        }
    }
    /// Reads an object token: one strong reference the caller must adopt
    /// into a wrapper whose deinit releases it.
    mutating func readObject() -> OpaquePointer {
        guard let p = OpaquePointer(bitPattern: UInt(readUInt64())) else {
            wvDecodeFailure("null object token")
        }
        return p
    }
    func finish() {
        if remaining != 0 { wvDecodeFailure("trailing bytes after value") }
    }
}

"#;

/// Append the private buffer runtime implementing the value-buffer wire
/// format.
pub(crate) fn render_buffer_runtime(out: &mut String) {
    out.push_str(BUFFER_RUNTIME);
}

/// Append the generic brand error enum plus the `check`/`trap` helpers every
/// wrapper body reports its error slot through.
///
/// The brand error covers unknown codes, marshalling failures, panics, and
/// foreign callback failures; typed domain errors get one enum per declaring
/// module, emitted alongside that module's types. Domain error codes are
/// validated positive, so the reserved negative runtime codes (generic `-1`,
/// panic `-2`, marshalling `-3`, foreign callback failure `-4`) always reach
/// the brand error on throwing paths and the `fatalError` trap on
/// non-throwing ones.
pub(crate) fn render_error_infra(out: &mut String) {
    out.push_str(&format!(
        "public enum {ERROR_BRAND}: Error, LocalizedError {{\n"
    ));
    out.push_str("    case error(code: Int32, message: String)\n");
    out.push_str("    public var errorDescription: String? {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(_, message): return message\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    public var errorCode: Int32 {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(code, _): return code\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc check(_ err: inout weaveffi_error) throws {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    out.push_str(&format!(
        "        throw {ERROR_BRAND}.error(code: code, message: message)\n"
    ));
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // The trapping flavor for non-throwing wrappers: a non-zero code here can
    // only be a producer panic, an argument-marshalling failure, or a
    // consumer callback implementation that threw.
    out.push_str("@inline(__always)\nfunc trap(_ err: inout weaveffi_error) {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    out.push_str("        fatalError(\"\\(code): \\(message)\")\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
}

/// Append the boxed-continuation class async wrappers thread through the C
/// `context` slot.
pub(crate) fn render_continuation_ref(out: &mut String) {
    // `E` is `Error` for a throwing async wrapper and `Never` for a plain
    // one, mirroring the checked-continuation flavor each uses.
    out.push_str("private final class ContinuationRef<T, E: Error> {\n");
    out.push_str("    let value: CheckedContinuation<T, E>\n");
    out.push_str("    init(_ value: CheckedContinuation<T, E>) { self.value = value }\n");
    out.push_str("}\n\n");
}

/// Append the callback-interface support: the helper every trampoline calls
/// when the consumer's implementation throws. It reports the failure through
/// the producer-owned `out_err` slot with `weaveffi_error_set`, which copies
/// the borrowed message, so no Swift allocation crosses the boundary and no
/// error ever unwinds through the C frame.
pub(crate) fn render_callback_support(out: &mut String) {
    out.push_str(
        "/// Reports a thrown Swift error to the producer as a foreign callback failure\n\
         /// (code -4); the producer aborts its current call with that code and message.\n\
         func wvForeignError(_ outErr: UnsafeMutablePointer<weaveffi_error>?, _ error: Error) {\n\
         \x20   let message = (error as? LocalizedError)?.errorDescription ?? String(describing: error)\n",
    );
    out.push_str(&format!(
        "    message.withCString {{ weaveffi_error_set(outErr, {FOREIGN_ERROR_CODE}, $0) }}\n"
    ));
    out.push_str("}\n\n");
}
