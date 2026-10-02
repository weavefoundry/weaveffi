// MARK: - Runtime support

/// A failure the native library reports outside any declared error domain:
/// an unknown error code or a runtime trap. Throwing wrappers raise it;
/// non-throwing wrappers stop the process with `fatalError` instead, since
/// Swift has no unchecked errors.
public struct {{RUNTIME_ERROR}}: Error, LocalizedError, Hashable, Sendable {
    /// The code of a generic runtime failure.
    public static let genericCode: Int32 = -1
    /// The code of a panic caught at the library boundary.
    public static let panicCode: Int32 = -2
    /// The code of an argument or result that failed to marshal.
    public static let marshalCode: Int32 = -3
    /// The code of a callback implementation that threw.
    public static let foreignCode: Int32 = -4

    /// The numeric code the native library reported.
    public let errorCode: Int32
    /// The message the native library reported.
    public let message: String

    /// Creates an error carrying a native code and message.
    public init(errorCode: Int32, message: String) {
        self.errorCode = errorCode
        self.message = message
    }

    public var errorDescription: String? { message }
}

/// The error slot every native call reports through.
typealias WvError = {{PREFIX}}_error

/// The C ABI revision these bindings were generated against.
let wvAbiVersion: UInt32 = {{ABI_VERSION}}

/// The contract check, run once before the first native call: the library
/// must implement this ABI revision and every top-level module must carry
/// the checksum these bindings were generated from.
let wvContract: Void = {
    let abi = {{PREFIX}}_abi_version()
    guard abi == wvAbiVersion else {
        fatalError("{{MODULE}}: the native library '{{LIBRARY}}' implements C ABI revision \(abi), but these bindings need revision \(wvAbiVersion)")
    }
{{CONTRACT_CHECKS}}
}()

/// Runs the contract check if it hasn't run yet.
@inline(__always)
func wvLoad() {
    _ = wvContract
}

/// Stops the process when a module's contract checksum doesn't match.
func wvCheckModule(_ module: StaticString, _ actual: UInt64, _ expected: UInt64) {
    guard actual == expected else {
        fatalError("{{MODULE}}: module '\(module)' of the native library '{{LIBRARY}}' doesn't match these bindings (checksum 0x\(String(actual, radix: 16)), expected 0x\(String(expected, radix: 16))); regenerate the bindings or rebuild the library")
    }
}

/// The message carried by an error slot.
func wvErrorMessage(_ err: WvError) -> String {
    err.message.map { String(cString: $0) } ?? ""
}

/// The Swift error for a code outside any declared domain: cancellation
/// (`-5`) becomes `CancellationError`, anything else the runtime error.
func wvRuntimeError(_ err: WvError) -> Error {
    if err.code == -5 {
        return CancellationError()
    }
    return {{RUNTIME_ERROR}}(errorCode: err.code, message: wvErrorMessage(err))
}

/// Stops the process for a failed call that can't throw, naming the code and
/// message the library reported.
func wvFatal(_ code: Int32, _ message: String, _ function: StaticString) -> Never {
    fatalError("{{MODULE}}.\(function) failed with code \(code): \(message)")
}

/// Throws the runtime error for a filled error slot, clearing it.
@inline(__always)
func wvCheck(_ err: inout WvError) throws {
    guard err.code != 0 else { return }
    let error = wvRuntimeError(err)
    {{PREFIX}}_error_clear(&err)
    throw error
}

/// Stops the process for a filled error slot of a call that can't throw.
@inline(__always)
func wvTrap(_ err: inout WvError, _ function: StaticString = #function) {
    guard err.code != 0 else { return }
    let code = err.code
    let message = wvErrorMessage(err)
    {{PREFIX}}_error_clear(&err)
    wvFatal(code, message, function)
}

/// Maps and releases the heap-boxed error an async completion received.
func wvTakeError(_ err: UnsafeMutablePointer<WvError>, _ map: (WvError) -> Error) -> Error {
    let error = map(err.pointee)
    {{PREFIX}}_error_free(err)
    return error
}

/// The error mapping of a cancellable call that declares no error domain:
/// cancellation throws `CancellationError`, anything else stops the process.
func wvCancelledOrTrap(_ err: WvError) -> Error {
    if err.code == -5 {
        return CancellationError()
    }
    wvFatal(err.code, wvErrorMessage(err), "async call")
}

/// Stops the process for the boxed error of an async call that can't throw.
func wvTrapBoxed(_ err: UnsafeMutablePointer<WvError>, _ function: StaticString = #function) -> Never {
    wvFatal(err.pointee.code, wvErrorMessage(err.pointee), function)
}

/// Unwraps a pointer the library promised is non-null.
@inline(__always)
func wvNonNull<P>(_ ptr: P?, _ function: StaticString = #function) -> P {
    guard let ptr = ptr else {
        wvFatal({{RUNTIME_ERROR}}.marshalCode, "the native library returned a null pointer", function)
    }
    return ptr
}

/// The Swift enum case for a C-style enum discriminant.
func wvEnumCase<E: RawRepresentable>(_ type: E.Type, _ raw: Int32) -> E where E.RawValue == Int32 {
    guard let value = E(rawValue: raw) else {
        wvDecodeFailure("unknown \(E.self) discriminant \(raw)")
    }
    return value
}

// MARK: Strings, bytes, and buffers

/// Lends the UTF-8 bytes of `s` (not NUL-terminated) for one call.
@inline(__always)
func wvWithUTF8<R>(_ s: String, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    var s = s
    return try s.withUTF8 { try body($0.baseAddress, $0.count) }
}

/// Lends the bytes of `d` for one call.
@inline(__always)
func wvWithBytes<R>(_ d: Data, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    try d.withUnsafeBytes { try body($0.baseAddress?.assumingMemoryBound(to: UInt8.self), $0.count) }
}

/// Lends an encoded value buffer for one call.
@inline(__always)
func wvWithBuffer<R>(_ w: WvWriter, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    try w.bytes.withUnsafeBufferPointer { try body($0.baseAddress, $0.count) }
}

/// Copies a returned string, then releases the library's allocation.
func wvTakeString(_ ptr: UnsafePointer<UInt8>?, _ len: Int) -> String {
    guard let ptr = ptr else { return "" }
    defer { {{PREFIX}}_free_bytes(UnsafeMutablePointer(mutating: ptr), len) }
    return String(decoding: UnsafeBufferPointer(start: ptr, count: len), as: UTF8.self)
}

/// Copies returned bytes, then releases the library's allocation.
func wvTakeBytes(_ ptr: UnsafePointer<UInt8>?, _ len: Int) -> Data {
    guard let ptr = ptr else { return Data() }
    defer { {{PREFIX}}_free_bytes(UnsafeMutablePointer(mutating: ptr), len) }
    return Data(bytes: ptr, count: len)
}

/// Decodes a returned value buffer in place, then releases it.
func wvTakeBuffer<T>(_ ptr: UnsafePointer<UInt8>?, _ len: Int, _ decode: (inout WvReader) -> T) -> T {
    defer {
        if let ptr = ptr { {{PREFIX}}_free_bytes(UnsafeMutablePointer(mutating: ptr), len) }
    }
    return wvBorrowBuffer(ptr, len, decode)
}

/// Copies a borrowed string.
func wvBorrowString(_ ptr: UnsafePointer<UInt8>?, _ len: Int) -> String {
    guard let ptr = ptr else { return "" }
    return String(decoding: UnsafeBufferPointer(start: ptr, count: len), as: UTF8.self)
}

/// Copies borrowed bytes.
func wvBorrowBytes(_ ptr: UnsafePointer<UInt8>?, _ len: Int) -> Data {
    guard let ptr = ptr else { return Data() }
    return Data(bytes: ptr, count: len)
}

/// Decodes a borrowed value buffer in place.
func wvBorrowBuffer<T>(_ ptr: UnsafePointer<UInt8>?, _ len: Int, _ decode: (inout WvReader) -> T) -> T {
    var r = WvReader(UnsafeRawPointer(ptr), len)
    let value = decode(&r)
    r.finish()
    return value
}

/// Stops the process on a malformed value buffer, which the wire format
/// treats like a marshalling failure.
func wvDecodeFailure(_ context: String) -> Never {
    fatalError("{{MODULE}}: malformed value buffer: \(context)")
}

/// Serializes values in the value-buffer wire format: little-endian, packed,
/// no alignment.
struct WvWriter {
    var bytes: [UInt8] = []

    init() {
        bytes.reserveCapacity(64)
    }

    @inline(__always)
    private mutating func append<T: FixedWidthInteger>(_ v: T) {
        withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) }
    }

    mutating func writeBool(_ v: Bool) { bytes.append(v ? 1 : 0) }
    mutating func writeI8(_ v: Int8) { append(v) }
    mutating func writeU8(_ v: UInt8) { append(v) }
    mutating func writeI16(_ v: Int16) { append(v) }
    mutating func writeU16(_ v: UInt16) { append(v) }
    mutating func writeI32(_ v: Int32) { append(v) }
    mutating func writeU32(_ v: UInt32) { append(v) }
    mutating func writeI64(_ v: Int64) { append(v) }
    mutating func writeU64(_ v: UInt64) { append(v) }
    mutating func writeF32(_ v: Float) { append(v.bitPattern) }
    mutating func writeF64(_ v: Double) { append(v.bitPattern) }

    /// Writes a `u32` length or element count.
    mutating func writeLen(_ n: Int) {
        precondition(n >= 0 && n <= Int(UInt32.max), "value buffer length exceeds UInt32.max")
        append(UInt32(n))
    }

    mutating func writeString(_ v: String) {
        let utf8 = v.utf8
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
    mutating func writeObject(_ p: OpaquePointer) { append(UInt64(UInt(bitPattern: p))) }
}

/// Deserializes values from the value-buffer wire format directly from the
/// library's memory, rejecting truncated buffers, invalid flag bytes,
/// oversized length prefixes, and trailing bytes.
struct WvReader {
    private let base: UnsafeRawPointer?
    private let count: Int
    private var pos = 0

    init(_ base: UnsafeRawPointer?, _ count: Int) {
        self.base = base
        self.count = base == nil ? 0 : count
    }

    /// The number of unread bytes.
    var remaining: Int { count - pos }

    private mutating func take(_ n: Int, _ what: StaticString) -> UnsafeRawPointer {
        guard n <= remaining, let base = base else { wvDecodeFailure("truncated \(what)") }
        defer { pos += n }
        return base + pos
    }

    @inline(__always)
    private mutating func load<T: FixedWidthInteger>(_ type: T.Type, _ what: StaticString) -> T {
        T(littleEndian: take(MemoryLayout<T>.size, what).loadUnaligned(as: T.self))
    }

    mutating func readBool() -> Bool {
        switch load(UInt8.self, "bool") {
        case 0: return false
        case 1: return true
        default: wvDecodeFailure("bool byte out of range")
        }
    }
    mutating func readI8() -> Int8 { load(Int8.self, "i8") }
    mutating func readU8() -> UInt8 { load(UInt8.self, "u8") }
    mutating func readI16() -> Int16 { load(Int16.self, "i16") }
    mutating func readU16() -> UInt16 { load(UInt16.self, "u16") }
    mutating func readI32() -> Int32 { load(Int32.self, "i32") }
    mutating func readU32() -> UInt32 { load(UInt32.self, "u32") }
    mutating func readI64() -> Int64 { load(Int64.self, "i64") }
    mutating func readU64() -> UInt64 { load(UInt64.self, "u64") }
    mutating func readF32() -> Float { Float(bitPattern: load(UInt32.self, "f32")) }
    mutating func readF64() -> Double { Double(bitPattern: load(UInt64.self, "f64")) }

    /// Reads a byte length, which can't exceed the unread bytes.
    mutating func readLen() -> Int {
        let n = Int(load(UInt32.self, "length"))
        guard n <= remaining else { wvDecodeFailure("length prefix exceeds remaining buffer") }
        return n
    }

    /// Reads an element count. Elements may encode to zero bytes, so the
    /// count isn't bounded by the unread bytes; callers cap preallocation.
    mutating func readCount() -> Int {
        Int(load(UInt32.self, "count"))
    }

    mutating func readString() -> String {
        let n = readLen()
        if n == 0 { return "" }
        let p = take(n, "string")
        guard let s = String(bytes: UnsafeRawBufferPointer(start: p, count: n), encoding: .utf8) else {
            wvDecodeFailure("string is not valid UTF-8")
        }
        return s
    }

    mutating func readBytes() -> Data {
        let n = readLen()
        if n == 0 { return Data() }
        return Data(bytes: take(n, "bytes"), count: n)
    }

    mutating func readOptionFlag() -> Bool {
        switch load(UInt8.self, "option flag") {
        case 0: return false
        case 1: return true
        default: wvDecodeFailure("option flag byte out of range")
        }
    }

    /// Reads an object token: one strong reference the caller adopts into a
    /// wrapper whose deinit releases it.
    mutating func readObject() -> OpaquePointer {
        guard let p = OpaquePointer(bitPattern: UInt(load(UInt64.self, "object token"))) else {
            wvDecodeFailure("null object token")
        }
        return p
    }

    func finish() {
        if remaining != 0 { wvDecodeFailure("trailing bytes after value") }
    }
}

// MARK: Async and callbacks

/// A native cancel token owned by one cancellable call. The library takes its
/// own reference at launch, so this one is released whenever the wrapper is
/// done with it.
final class WvCancelToken: @unchecked Sendable {
    let raw: OpaquePointer

    init() {
        raw = wvNonNull({{PREFIX}}_cancel_token_create())
    }

    deinit {
        {{PREFIX}}_cancel_token_destroy(raw)
    }

    func cancel() {
        {{PREFIX}}_cancel_token_cancel(raw)
    }
}

/// A continuation boxed for the C `context` slot of an async launch.
final class WvContinuation<T, E: Error> {
    let value: CheckedContinuation<T, E>

    init(_ value: CheckedContinuation<T, E>) {
        self.value = value
    }
}

/// One process-wide callback vtable at a stable address the library may hold
/// for the life of the process.
final class WvVtable<T>: @unchecked Sendable {
    let pointer: UnsafePointer<T>

    init(_ value: T) {
        let cell = UnsafeMutablePointer<T>.allocate(capacity: 1)
        cell.initialize(to: value)
        pointer = UnsafePointer(cell)
    }
}

/// Reports a thrown Swift error to the library as a callback failure
/// (code -4); the library aborts its current call with that code and message.
func wvForeignError(_ outErr: UnsafeMutablePointer<WvError>?, _ error: Error) {
    let message = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
    message.withCString { {{PREFIX}}_error_set(outErr, {{RUNTIME_ERROR}}.foreignCode, $0) }
}
